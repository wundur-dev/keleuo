//! `workbench-hook <socket> <token> <event>`: the command an agent's terminal interface runs on
//! its hook events while Workbench drives the session. It passes what the agent wrote on stdin
//! to the window over the window's local socket, as one line:
//! `{"token": ..., "event": ..., "payload": <stdin>}`. For a permission request it waits for
//! the window's answer and prints it; for anything else it returns at once. When the window is
//! not there, or closes the connection without answering, it prints nothing, so the agent
//! carries on as without the hook (its own interface asks).
//!
//! Standard library only, so it starts in a few milliseconds.

use std::io::{BufRead, BufReader, Read, Write};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(socket), Some(token), Some(event)) = (args.get(1), args.get(2), args.get(3)) else {
        eprintln!("usage: workbench-hook <socket> <token> <event>");
        return;
    };
    let mut payload = String::new();
    let _ = std::io::stdin().read_to_string(&mut payload);
    let payload = payload.trim().replace(['\n', '\r'], " ");
    let payload = if payload.is_empty() { "null".to_string() } else { payload };
    if let Some(answer) = ask(socket, token, event, &payload, event == "PermissionRequest") {
        print!("{answer}");
        let _ = std::io::stdout().flush();
    }
}

#[cfg(unix)]
fn ask(socket: &str, token: &str, event: &str, payload: &str, wait: bool) -> Option<String> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).ok()?;
    let line = format!("{{\"token\":{},\"event\":{},\"payload\":{payload}}}\n", quote(token), quote(event));
    stream.write_all(line.as_bytes()).ok()?;
    if !wait {
        return None;
    }
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).ok()?;
    let answer = answer.trim();
    (!answer.is_empty()).then(|| answer.to_string())
}

#[cfg(not(unix))]
fn ask(_: &str, _: &str, _: &str, _: &str, _: bool) -> Option<String> {
    None
}

/// A JSON string of plain text (tokens and event names are letters, digits and dashes).
fn quote(s: &str) -> String {
    let clean: String = s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    format!("\"{clean}\"")
}
