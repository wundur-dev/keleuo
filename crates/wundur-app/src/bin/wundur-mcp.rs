//! `wundur-mcp <socket> <token>`: the tool server (MCP, over stdin and stdout) an agent is
//! started with while Wundur drives its session. Its tools reach the other sessions in the
//! window: `list_sessions`, `send_message` and `ask_for_review`, and the tools the person made
//! from command-line programs in Settings (asked of the window when the agent lists them). It knows only what the tools are called and what
//! they take; each call goes to the window over its local socket as one line,
//! `{"token": ..., "event": "mcp", "payload": {"tool": ..., "arguments": ...}}`, and the
//! window's answer, `{"text": ..., "error": bool}`, is the call's result. With the window gone
//! a call says so.
//!
//! MCP over stdio is JSON-RPC 2.0, one message per line.

use std::io::{BufRead, BufReader, Write};

use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{GenericFilePath, Stream, ToFsName};
use serde_json::{Value, json};

/// The protocol version answered when the agent asks for one this does not know.
const PROTOCOL: &str = "2025-06-18";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(socket), Some(token)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: wundur-mcp <socket> <token>");
        return;
    };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        // Notifications (no id) need no answer.
        let Some(id) = msg.get("id").cloned() else { continue };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let reply = match method {
            "initialize" => {
                let version = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL);
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "wundur", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "Tools to reach the other sessions open in Wundur (other agents, in other folders). Messages you send are shown to the person, who may need to allow them before they go.",
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => {
                // Wundur's own, then the person's (from the window; none when it is not there).
                let mut all = tools().as_array().cloned().unwrap_or_default();
                let theirs = ask(socket, token, &json!({ "list": true }));
                all.extend(theirs.and_then(|v| v.get("tools").and_then(Value::as_array).cloned()).unwrap_or_default());
                Ok(json!({ "tools": all }))
            }
            "tools/call" => {
                let tool = params.get("name").and_then(Value::as_str).unwrap_or_default();
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(call(socket, token, tool, &arguments))
            }
            _ => Err(json!({ "code": -32601, "message": format!("unknown method {method}") })),
        };
        let answer = match reply {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
        };
        if writeln!(out, "{answer}").and_then(|_| out.flush()).is_err() {
            break;
        }
    }
}

/// The tools, as MCP describes them.
fn tools() -> Value {
    json!([
        {
            "name": "list_sessions",
            "description": "List the other sessions open in Wundur: each one's name, its agent and its folder. Use a name from here with send_message.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "send_message",
            "description": "Send a message to another session open in Wundur, by its name (see list_sessions). Its agent gets it as a new turn, marked as coming from this session (not from the person); it can answer with send_message to this session. Use it to hand work over (\"the release is out: update the download page\") or to ask for something. The person sees every message and may have to allow it first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "The other session's name, as list_sessions gives it." },
                    "message": { "type": "string", "description": "What to say: complete on its own, since the other agent has not seen this conversation." },
                },
                "required": ["to", "message"],
            },
        },
        {
            "name": "notify",
            "description": "Tell the person something in Wundur's inbox: the one place they see notices from all their sessions at once. Use it when something deserves their attention while they may be looking elsewhere: a long task is done, a decision waits on them, or something went wrong that they should know about. One notice per thing worth their attention, not progress updates; it doesn't wait for an answer. Clicking it opens this session.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "A short headline, under about 60 characters: \"Release 0.2.4 is built\", \"Pick a database for the save system\"." },
                    "message": { "type": "string", "description": "Optional detail, a sentence or two." },
                    "kind": { "type": "string", "enum": ["info", "done", "needs_you", "problem"], "description": "What it is about: done (finished), needs_you (a decision or input waits on the person), problem (something went wrong), or info." },
                    "choices": { "type": "array", "items": { "type": "string" }, "description": "For needs_you: up to six short answers to offer as buttons (\"Cloudflare Pages\", \"The VPS\"); the person can also write their own. Their answer comes to this session as their next message." },
                },
                "required": ["title"],
            },
        },
        {
            "name": "ask_for_review",
            "description": "Ask another agent for a second opinion on work in this folder: Wundur opens a new, read-only session of that agent here (it can read and run read-only commands, not change files), gives it your request, and its review comes back to this session as a message. The person allows it first. Don't wait for it: finish your turn; the review arrives as a new message.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "agent": { "type": "string", "description": "Which agent reviews: \"Claude Code\" or \"Cursor\". Left out: a different agent from this one, when there is one." },
                    "request": { "type": "string", "description": "What to review and what to look for, complete on its own (the reviewer has not seen this conversation): the files or the change, the concerns, what kind of answer you want." },
                },
                "required": ["request"],
            },
        },
    ])
}

/// One message to the window, its answer (`None` when it is not there).
fn ask(socket: &str, token: &str, payload: &Value) -> Option<Value> {
    let mut stream = Stream::connect(socket.to_fs_name::<GenericFilePath>().ok()?).ok()?;
    let line = json!({ "token": token, "event": "mcp", "payload": payload });
    stream.write_all(format!("{line}\n").as_bytes()).ok()?;
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).ok()?;
    serde_json::from_str(answer.trim()).ok()
}

/// One tool call, answered by the window (`{"text", "error", "images": [{"data", "mimeType"}]}`),
/// as MCP's result.
fn call(socket: &str, token: &str, tool: &str, arguments: &Value) -> Value {
    match ask(socket, token, &json!({ "tool": tool, "arguments": arguments })) {
        Some(v) => {
            let text = v.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
            let mut content = vec![json!({ "type": "text", "text": text })];
            for image in v.get("images").and_then(Value::as_array).cloned().unwrap_or_default() {
                content.push(json!({ "type": "image", "data": image["data"], "mimeType": image["mimeType"] }));
            }
            json!({ "content": content, "isError": v.get("error").and_then(Value::as_bool).unwrap_or(false) })
        }
        None => json!({ "content": [{ "type": "text", "text": "Wundur is not answering (its window may have closed)." }], "isError": true }),
    }
}
