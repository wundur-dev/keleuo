//! A toolkit for agents that speak ACP, the Agent Client Protocol (JSON-RPC 2.0, one message
//! per line on the agent's stdin and stdout). It is not a layer every agent goes through: a
//! backend that uses it ([`crate::backends::cursor`]) still owns its agent, maps what ACP
//! standardises with [`map`], and handles its agent's own extensions itself. Agents with a
//! richer interface of their own (Claude Code, Codex) keep it.
//!
//! [`Connection`] runs the agent and carries messages: requests from here wait for their
//! response on a channel; what the agent sends (notifications, and requests it waits on, such
//! as asking permission) goes to the handler given at start.

pub mod map;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::host::CommandSpec;

/// The protocol version spoken.
pub const PROTOCOL_VERSION: u64 = 1;

/// What the agent sends that is not a response to a request from here.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    /// A message expecting no answer (`session/update`, ...).
    Notification { method: String, params: Value },
    /// A request the agent waits on (`session/request_permission`, an extension); answer it
    /// with [`Connection::respond`] or [`Connection::respond_error`].
    Request { id: Value, method: String, params: Value },
    /// A line the agent wrote to its error output.
    Log(String),
    /// The agent's output ended: it has exited (or is about to).
    Closed,
}

/// The error code ACP gives a request the agent refuses until it is signed in.
pub const AUTH_REQUIRED: i64 = -32000;

/// Whether `e` is the agent asking to be signed in first.
pub fn needs_sign_in(e: &anyhow::Error) -> bool {
    e.downcast_ref::<RpcError>().is_some_and(|r| r.code == AUTH_REQUIRED)
}

/// An error response.
#[derive(Clone, Debug, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

/// JSON-RPC's code for a method the receiver does not have.
pub const METHOD_NOT_FOUND: i64 = -32601;

/// The answer to a request: its result, or an error (the agent's, or [`RpcError`] code 0 when
/// the agent went away first).
pub type Reply = Result<Value, RpcError>;

type Pending = Arc<Mutex<HashMap<u64, Sender<Reply>>>>;

/// A running ACP agent.
pub struct Connection {
    child: Mutex<Option<Child>>,
    out: Sender<String>,
    pending: Pending,
    next: AtomicU64,
}

impl Connection {
    /// Start `spec` (the agent in its ACP mode); what it sends goes to `handler`, on a reader
    /// thread.
    pub fn spawn(spec: &CommandSpec, handler: impl Fn(Incoming) + Send + Sync + 'static) -> anyhow::Result<Self> {
        let mut cmd = spec.to_command();
        // A group of its own: an agent may start workers that must end with it.
        crate::platform::own_group(&mut cmd);
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| anyhow::anyhow!("could not start {}: {e}", spec.program.display()))?;
        let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let handler: Arc<dyn Fn(Incoming) + Send + Sync> = Arc::new(handler);
        let pending: Pending = Arc::default();
        // Writes in order, off the caller's thread (a prompt with images can be large).
        let (out, lines) = channel::<String>();
        if let Some(mut stdin) = stdin {
            std::thread::spawn(move || {
                for line in lines {
                    if stdin.write_all(line.as_bytes()).and_then(|()| stdin.write_all(b"\n")).and_then(|()| stdin.flush()).is_err() {
                        break;
                    }
                }
            });
        }
        if let Some(stderr) = stderr {
            let h = handler.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    h(Incoming::Log(line));
                }
            });
        }
        if let Some(stdout) = stdout {
            let (h, p) = (handler.clone(), pending.clone());
            std::thread::spawn(move || read_messages(stdout, &p, h.as_ref()));
        }
        Ok(Self { child: Mutex::new(Some(child)), out, pending, next: AtomicU64::new(1) })
    }

    /// Send a request; its reply arrives on the returned channel.
    pub fn request(&self, method: &str, params: Value) -> Receiver<Reply> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = channel();
        self.pending.lock().unwrap().insert(id, tx);
        if self.out.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()).is_err()
            && let Some(tx) = self.pending.lock().unwrap().remove(&id)
        {
            let _ = tx.send(Err(gone()));
        }
        rx
    }

    /// Send a request and wait for its reply (not on the window's thread).
    pub fn call(&self, method: &str, params: Value) -> Reply {
        self.request(method, params).recv().unwrap_or_else(|_| Err(gone()))
    }

    /// Send a notification (no reply).
    pub fn notify(&self, method: &str, params: Value) {
        let _ = self.out.send(json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string());
    }

    /// Answer the agent's request `id`.
    pub fn respond(&self, id: &Value, result: Value) {
        let _ = self.out.send(json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string());
    }

    /// Answer the agent's request `id` with an error.
    pub fn respond_error(&self, id: &Value, code: i64, message: &str) {
        let _ = self.out.send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string());
    }

    /// The agent process's id, while it runs.
    pub fn pid(&self) -> Option<u32> {
        self.child.lock().unwrap().as_ref().map(Child::id)
    }

    /// End the agent and what it started.
    pub fn kill(&self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            crate::platform::kill_group(&mut child);
            let _ = child.wait();
        }
        for (_, tx) in self.pending.lock().unwrap().drain() {
            let _ = tx.send(Err(gone()));
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.kill();
    }
}

fn gone() -> RpcError {
    RpcError { code: 0, message: "the agent is not running".into(), data: None }
}

/// Read messages until the output ends: responses to their waiting requests, the rest to
/// `handler`; then every request still waiting gets [`gone`].
fn read_messages(stdout: impl Read, pending: &Pending, handler: &(dyn Fn(Incoming) + Send + Sync)) {
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            handler(Incoming::Log(line.to_string()));
            continue;
        };
        match classify(v) {
            Some(Message::Response(id, reply)) => {
                if let Some(tx) = pending.lock().unwrap().remove(&id) {
                    let _ = tx.send(reply);
                }
            }
            Some(Message::Incoming(incoming)) => handler(incoming),
            None => handler(Incoming::Log(line.to_string())),
        }
    }
    for (_, tx) in pending.lock().unwrap().drain() {
        let _ = tx.send(Err(gone()));
    }
    handler(Incoming::Closed);
}

/// One parsed message.
#[derive(Debug, PartialEq)]
enum Message {
    Response(u64, Reply),
    Incoming(Incoming),
}

/// What a JSON-RPC message is: a response (to a numeric id of ours), a request or a
/// notification. `None` for anything else.
fn classify(mut v: Value) -> Option<Message> {
    let method = v.get("method").and_then(Value::as_str).map(str::to_string);
    let params = v.get_mut("params").map(Value::take).unwrap_or(Value::Null);
    match (method, v.get("id").cloned()) {
        (Some(method), Some(id)) => Some(Message::Incoming(Incoming::Request { id, method, params })),
        (Some(method), None) => Some(Message::Incoming(Incoming::Notification { method, params })),
        (None, Some(id)) => {
            let id = id.as_u64()?;
            let reply = match v.get_mut("error").map(Value::take) {
                Some(e) => Err(RpcError {
                    code: e.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: e.get("message").and_then(Value::as_str).unwrap_or_default().to_string(),
                    data: e.get("data").cloned(),
                }),
                None => Ok(v.get_mut("result").map(Value::take).unwrap_or(Value::Null)),
            };
            Some(Message::Response(id, reply))
        }
        (None, None) => None,
    }
}

/// The `initialize` request's parameters: this protocol version, what Keleuo can do for the
/// agent (it reads and writes files itself, and runs its own commands: none offered), and who
/// is asking.
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
        "clientInfo": {"name": "keleuo", "version": env!("CARGO_PKG_VERSION")},
    })
}

/// A prompt's content: images first (base64, from files on this machine), then the text.
pub fn prompt_blocks(text: &str, images: &[std::path::PathBuf]) -> std::io::Result<Vec<Value>> {
    use base64::Engine as _;
    let mut blocks = Vec::new();
    for path in images {
        let data = base64::engine::general_purpose::STANDARD.encode(std::fs::read(path)?);
        let mime = match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
            Some("jpg" | "jpeg") => "image/jpeg",
            Some("gif") => "image/gif",
            Some("webp") => "image/webp",
            _ => "image/png",
        };
        blocks.push(json!({"type": "image", "mimeType": mime, "data": data}));
    }
    if !text.is_empty() {
        blocks.push(json!({"type": "text", "text": text}));
    }
    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_to_sign_in_first_is_known_by_its_code() {
        let e: anyhow::Error = RpcError { code: AUTH_REQUIRED, message: "Authentifizierung erforderlich".into(), data: None }.into();
        assert!(needs_sign_in(&e));
        let other: anyhow::Error = RpcError { code: -32603, message: "Authentication required".into(), data: None }.into();
        assert!(!needs_sign_in(&other), "the words alone are no sign");
    }

    #[test]
    fn messages_are_told_apart() {
        let ok = classify(json!({"jsonrpc": "2.0", "id": 3, "result": {"sessionId": "s"}}));
        assert_eq!(ok, Some(Message::Response(3, Ok(json!({"sessionId": "s"})))));
        let err = classify(json!({"jsonrpc": "2.0", "id": 4, "error": {"code": -32601, "message": "no"}}));
        assert!(matches!(err, Some(Message::Response(4, Err(RpcError { code: METHOD_NOT_FOUND, .. })))));
        let note = classify(json!({"jsonrpc": "2.0", "method": "session/update", "params": {"x": 1}}));
        assert_eq!(note, Some(Message::Incoming(Incoming::Notification { method: "session/update".into(), params: json!({"x": 1}) })));
        let ask = classify(json!({"jsonrpc": "2.0", "id": "a1", "method": "session/request_permission", "params": {}}));
        assert!(matches!(ask, Some(Message::Incoming(Incoming::Request { ref method, .. })) if method == "session/request_permission"));
        // A response to an id we never use (not a number) is not ours.
        assert_eq!(classify(json!({"jsonrpc": "2.0", "id": "x", "result": 1})), None);
    }

    /// A fake agent (a shell loop) answering every request with its own params: requests and
    /// responses meet by id, notifications from it reach the handler, and its end is reported.
    #[cfg(unix)]
    #[test]
    fn requests_get_their_replies() {
        let script = r#"while IFS= read -r line; do
            id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
            printf '{"jsonrpc":"2.0","method":"session/update","params":{"seen":%s}}\n' "$id"
            printf '{"jsonrpc":"2.0","id":%s,"result":{"echo":%s}}\n' "$id" "$id"
        done"#;
        let spec = CommandSpec { program: "/bin/sh".into(), args: vec!["-c".into(), script.into()], cwd: None, env: Vec::new() };
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        let conn = Connection::spawn(&spec, move |m| s.lock().unwrap().push(m)).unwrap();
        let a = conn.request("one", json!({}));
        let b = conn.request("two", json!({}));
        assert_eq!(b.recv().unwrap(), Ok(json!({"echo": 2})));
        assert_eq!(a.recv().unwrap(), Ok(json!({"echo": 1})));
        conn.kill();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let seen = seen.lock().unwrap();
        assert!(seen.iter().any(|m| matches!(m, Incoming::Notification { params, .. } if params == &json!({"seen": 1}))));
        assert!(seen.iter().any(|m| matches!(m, Incoming::Closed)));
    }

    /// The toolkit against a real ACP agent (Cursor's, signed in): start, open a session,
    /// prompt, and the reply comes through the standard mapping.
    /// `cargo test -p keleuo-core acp_for_real -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn acp_for_real() {
        let dir = std::env::temp_dir().join(format!("keleuo-acp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let spec = crate::host::Host::program_command(
            &crate::host::LocalHost,
            &crate::host::ProgramSpec { program: "cursor-agent".into(), args: vec!["acp".into()], cwd: dir.clone(), ..Default::default() },
        );
        let (tx, rx) = channel::<Incoming>();
        let tx = Mutex::new(tx);
        let conn = Connection::spawn(&spec, move |m| drop(tx.lock().unwrap().send(m))).unwrap();
        let init = conn.call("initialize", initialize_params()).unwrap();
        assert_eq!(init["protocolVersion"], PROTOCOL_VERSION);
        let session = conn.call("session/new", json!({"cwd": dir, "mcpServers": []})).unwrap();
        let sid = session["sessionId"].as_str().unwrap().to_string();
        let prompt = conn.request("session/prompt", json!({"sessionId": sid, "prompt": prompt_blocks("Reply with just the word: pong", &[]).unwrap()}));
        let reply = prompt.recv_timeout(std::time::Duration::from_secs(120)).expect("the prompt ends");
        let mut mapper = map::Mapper::default();
        let mut text = String::new();
        while let Ok(m) = rx.try_recv() {
            if let Incoming::Notification { method, params } = m
                && method == "session/update"
            {
                for e in mapper.update(&params["update"]).unwrap_or_default() {
                    if let crate::agent::Event::TextDelta(t) = e {
                        text.push_str(&t);
                    }
                }
            }
        }
        eprintln!("reply: {text:?}, end: {:?}", map::turn_ended(&reply, &sid));
        assert!(text.to_lowercase().contains("pong"));
        assert!(matches!(map::turn_ended(&reply, &sid), crate::agent::Event::TurnEnded { error: None, .. }));
        conn.kill();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
