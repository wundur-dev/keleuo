//! `keleuo-mcp <socket> <token>`: the tool server (MCP, over stdin and stdout) an agent is
//! started with while Keleuo drives its session. Its tools reach the other sessions in the
//! window: `list_sessions` and `send_message`. It knows only what the tools are called and what
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
        eprintln!("usage: keleuo-mcp <socket> <token>");
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
                    "serverInfo": { "name": "keleuo", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "Tools to reach the other sessions open in Keleuo (other agents, in other folders). Messages you send are shown to the person, who may need to allow them before they go.",
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => {
                let tool = params.get("name").and_then(Value::as_str).unwrap_or_default();
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
                let (text, error) = call(socket, token, tool, &arguments);
                Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": error }))
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
            "description": "List the other sessions open in Keleuo: each one's name, its agent and its folder. Use a name from here with send_message.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "send_message",
            "description": "Send a message to another session open in Keleuo, by its name (see list_sessions). Its agent gets it as a new turn, marked as coming from this session (not from the person); it can answer with send_message to this session. Use it to hand work over (\"the release is out: update the download page\") or to ask for something. The person sees every message and may have to allow it first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "The other session's name, as list_sessions gives it." },
                    "message": { "type": "string", "description": "What to say: complete on its own, since the other agent has not seen this conversation." },
                },
                "required": ["to", "message"],
            },
        },
    ])
}

/// One tool call, answered by the window: its text, and whether it failed.
fn call(socket: &str, token: &str, tool: &str, arguments: &Value) -> (String, bool) {
    let ask = || -> Option<Value> {
        let mut stream = Stream::connect(socket.to_fs_name::<GenericFilePath>().ok()?).ok()?;
        let line = json!({ "token": token, "event": "mcp", "payload": { "tool": tool, "arguments": arguments } });
        stream.write_all(format!("{line}\n").as_bytes()).ok()?;
        let mut answer = String::new();
        BufReader::new(stream).read_line(&mut answer).ok()?;
        serde_json::from_str(answer.trim()).ok()
    };
    match ask() {
        Some(v) => (
            v.get("text").and_then(Value::as_str).unwrap_or_default().to_string(),
            v.get("error").and_then(Value::as_bool).unwrap_or(false),
        ),
        None => ("Keleuo is not answering (its window may have closed).".into(), true),
    }
}
