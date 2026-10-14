//! `schemaVersion` 1, worked out against Cursor Agent 2026.10.14 (and seen in every chat of
//! 2026.09 and 2026.10 on the machine it was written on).
//!
//! `store.db` has two tables. `meta` holds one row, key `0`, whose value is JSON written out as
//! hex: `agentId`, `name`, `mode`, `createdAt` and `latestRootBlobId` (empty for a chat never
//! written to). `blobs` holds records by id: the SHA-256 of the record, in hex. A turn is
//! written when it ends, so one under way is not there yet. The root record is protobuf: each field 1 is the 32-byte id of one
//! message, in conversation order (other fields: token counts and the like). A message is JSON
//! in the Vercel AI SDK's form: `role` (`system`, `user`, `assistant`, `tool`) and `content`,
//! a string or a list of parts (`text`, `reasoning`, `tool-call` with `toolCallId`, `toolName`
//! and `args`, `tool-result` with `toolCallId` and `result`). The person's own words are the
//! `<user_query>` of a user message; user messages without one are context Cursor adds (the
//! machine, the folder). Other records (file contents kept for checkpoints, per-request
//! notes) are not part of the conversation.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::{Field, StoreReader, protobuf_fields};
use crate::agent::Event;
use crate::diff;

pub struct V1;

impl StoreReader for V1 {
    fn schema(&self) -> u64 {
        1
    }

    fn built_against(&self) -> &'static str {
        "cursor-agent 2026.10.14-0f9edde"
    }

    fn events(&self, db: &rusqlite::Connection, cwd: &str) -> anyhow::Result<Vec<Event>> {
        let meta_hex: String = db.query_row("SELECT value FROM meta WHERE key = '0'", [], |r| r.get(0))?;
        let meta: Value = serde_json::from_slice(&hex(&meta_hex)?)?;
        let root_id = meta
            .get("latestRootBlobId")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("meta has no latestRootBlobId"))?;
        // A chat made but never written to has no root yet.
        if root_id.is_empty() {
            return Ok(Vec::new());
        }
        let blob = |id: &str| -> anyhow::Result<Vec<u8>> {
            Ok(db.query_row("SELECT data FROM blobs WHERE id = ?1", [id], |r| r.get::<_, Vec<u8>>(0))?)
        };
        let root = blob(root_id)?;
        let fields = protobuf_fields(&root).ok_or_else(|| anyhow::anyhow!("the root record is not protobuf"))?;
        let mut messages = Vec::new();
        for (field, _, value) in fields {
            if let (1, Field::Bytes(id)) = (field, value) {
                anyhow::ensure!(id.len() == 32, "a message reference is {} bytes, not 32", id.len());
                let text = blob(&to_hex(id))?;
                messages.push(serde_json::from_slice::<Value>(&text)?);
            }
        }
        Ok(events(&messages, cwd))
    }
}

/// The messages as neutral events.
fn events(messages: &[Value], cwd: &str) -> Vec<Event> {
    let mut out = Vec::new();
    // Tool calls by id, for their results: the name and input given to the call.
    let mut calls: HashMap<String, (String, Value)> = HashMap::new();
    for m in messages {
        let parts = match m.get("content") {
            Some(Value::Array(parts)) => parts.as_slice(),
            // A string: the system prompt or context Cursor adds, not the conversation.
            _ => continue,
        };
        match m.get("role").and_then(Value::as_str) {
            Some("user") => {
                let text: Vec<&str> = parts.iter().filter_map(|p| user_query(s(p, "text"))).collect();
                if !text.is_empty() {
                    out.push(Event::UserText(text.join("\n\n")));
                }
            }
            Some("assistant") => {
                for p in parts {
                    match s(p, "type") {
                        "text" if !s(p, "text").trim().is_empty() => {
                            out.push(Event::Text { text: s(p, "text").to_string(), parent: None });
                        }
                        "tool-call" => {
                            let id = s(p, "toolCallId").to_string();
                            let (name, input) = tool(s(p, "toolName"), p.get("args").unwrap_or(&Value::Null));
                            let summary = summary(&name, &input, cwd);
                            let diff = diff::from_input(&name, &input);
                            calls.insert(id.clone(), (name.clone(), input.clone()));
                            out.push(Event::ToolCall { id, name, summary, input, diff, parent: None });
                        }
                        _ => {}
                    }
                }
            }
            Some("tool") => {
                for p in parts.iter().filter(|p| s(p, "type") == "tool-result") {
                    let id = s(p, "toolCallId").to_string();
                    let output = match p.get("result") {
                        Some(Value::String(t)) => t.clone(),
                        Some(v) => v.to_string(),
                        None => String::new(),
                    };
                    let (name, input) = calls.get(&id).cloned().unwrap_or_default();
                    let is_error = failed(&name, &output);
                    let file_changed = (diff::is_edit_tool(&name) && !is_error)
                        .then(|| input.get("file_path").and_then(Value::as_str).map(PathBuf::from))
                        .flatten();
                    out.push(Event::ToolResult { id, output, is_error, diff: None, file_changed, parent: None });
                }
            }
            _ => {}
        }
    }
    out
}

/// The person's words in a user message part: what is inside `<user_query>`.
fn user_query(text: &str) -> Option<&str> {
    let start = text.find("<user_query>")? + "<user_query>".len();
    let end = text[start..].find("</user_query>").map_or(text.len(), |e| start + e);
    Some(text[start..end].trim())
}

/// A Cursor tool call in the names Keleuo's cards and diffs know: `StrReplace` is an Edit
/// (`old_string`, `new_string`), `Write` a Write (`content`), with the file as `file_path`.
fn tool(name: &str, args: &Value) -> (String, Value) {
    let mut input: Map<String, Value> = args.as_object().cloned().unwrap_or_default();
    if let Some(path) = input.remove("path") {
        input.insert("file_path".into(), path);
    }
    let name = match name {
        "StrReplace" => "Edit",
        "Write" => {
            if let Some(contents) = input.remove("contents") {
                input.insert("content".into(), contents);
            }
            "Write"
        }
        other => other,
    };
    (name.to_string(), json!(input))
}

/// What a tool card says: the file (from the folder), the command or the pattern.
fn summary(name: &str, input: &Value, cwd: &str) -> String {
    if name == "Shell" {
        return s(input, "command").to_string();
    }
    for key in ["file_path", "pattern", "glob_pattern"] {
        let value = s(input, key);
        if !value.is_empty() {
            return value.strip_prefix(cwd).and_then(|p| p.strip_prefix('/')).unwrap_or(value).to_string();
        }
    }
    String::new()
}

/// Whether a result says the call failed: a command that exited with another code than 0.
fn failed(name: &str, output: &str) -> bool {
    name == "Shell"
        && output
            .strip_prefix("Exit code: ")
            .and_then(|r| r.split_whitespace().next())
            .is_some_and(|code| code != "0")
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn hex(text: &str) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(text.len().is_multiple_of(2), "odd hex");
    (0..text.len()).step_by(2).map(|i| Ok(u8::from_str_radix(&text[i..i + 2], 16)?)).collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::conversation::Item;

    /// A chat database laid out as Cursor 2026.10 writes it, from `messages` (message records,
    /// in order). The messages in the tests are cut from a real chat, paths changed.
    pub fn write_store(path: &std::path::Path, messages: &[Value]) {
        let db = rusqlite::Connection::open(path).unwrap();
        db.execute_batch("CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB); CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);").unwrap();
        let mut root = Vec::new();
        for (n, m) in messages.iter().enumerate() {
            let mut id = [0u8; 32];
            id[..8].copy_from_slice(&(n as u64 + 1).to_be_bytes());
            db.execute("INSERT INTO blobs VALUES (?1, ?2)", rusqlite::params![to_hex(&id), m.to_string().into_bytes()]).unwrap();
            root.extend_from_slice(&[0x0a, 32]);
            root.extend_from_slice(&id);
        }
        // Other fields of the root, as Cursor has them: a nested message with numbers.
        root.extend_from_slice(&[0x2a, 0x05, 0x08, 0x96, 0x01, 0x10, 0x05]);
        let root_id = "ff".repeat(32);
        db.execute("INSERT INTO blobs VALUES (?1, ?2)", rusqlite::params![root_id, root]).unwrap();
        let meta = json!({"agentId": "c1", "latestRootBlobId": root_id, "name": "New Agent", "mode": "default"});
        db.execute("INSERT INTO meta VALUES ('0', ?1)", [to_hex(meta.to_string().as_bytes())]).unwrap();
    }

    pub fn sample() -> Vec<Value> {
        vec![
            json!({"role": "system", "content": "You are an AI coding assistant..."}),
            json!({"role": "user", "content": "<user_info>\nWorkspace Path: /tmp/p\n</user_info>"}),
            json!({"role": "user", "content": [{"type": "text", "text": "<timestamp>Wednesday, Oct 14, 2026, 4:13 PM (UTC-4)</timestamp>\n<user_query>\nRead hello.txt, then create bye.txt containing the word bye, then run the shell command: ls\n</user_query>"}]}),
            json!({"role": "assistant", "id": "a1", "content": [
                {"type": "reasoning", "text": "", "signature": "x"},
                {"type": "text", "text": "Reading `hello.txt`, creating `bye.txt`, then listing the directory."},
                {"type": "tool-call", "toolCallId": "call-1\nfc_1", "toolName": "Read", "args": {"path": "/tmp/p/hello.txt"}}]}),
            json!({"role": "tool", "id": "t1", "content": [{"type": "tool-result", "toolCallId": "call-1\nfc_1", "toolName": "Read", "result": "hello\n"}]}),
            json!({"role": "assistant", "id": "a2", "content": [
                {"type": "tool-call", "toolCallId": "call-2", "toolName": "Write", "args": {"path": "/tmp/p/bye.txt", "contents": "bye\n"}},
                {"type": "tool-call", "toolCallId": "call-3", "toolName": "StrReplace", "args": {"path": "/tmp/p/hello.txt", "old_string": "hello", "new_string": "hi"}},
                {"type": "tool-call", "toolCallId": "call-4", "toolName": "Shell", "args": {"command": "ls", "description": "List directory contents"}}]}),
            json!({"role": "tool", "id": "t2", "content": [
                {"type": "tool-result", "toolCallId": "call-2", "toolName": "Write", "result": "Wrote contents to /tmp/p/bye.txt"},
                {"type": "tool-result", "toolCallId": "call-3", "toolName": "StrReplace", "result": "The file /tmp/p/hello.txt has been updated."},
                {"type": "tool-result", "toolCallId": "call-4", "toolName": "Shell", "result": "Exit code: 0\n\nCommand output:\n\n```\nbye.txt\nhello.txt\n\n```"}]}),
            json!({"role": "assistant", "id": "a3", "content": [{"type": "reasoning", "text": ""}, {"type": "text", "text": "`hello.txt` contains `hello`.\n\nCreated `bye.txt` with `bye`."}]}),
            json!({"role": "user", "content": [{"type": "text", "text": "<timestamp>Wednesday, Oct 14, 2026, 4:14 PM (UTC-4)</timestamp>\n<user_query>\nRun: false\n</user_query>"}]}),
            json!({"role": "assistant", "id": "a4", "content": [{"type": "tool-call", "toolCallId": "call-5", "toolName": "Shell", "args": {"command": "false"}}]}),
            json!({"role": "tool", "id": "t3", "content": [{"type": "tool-result", "toolCallId": "call-5", "toolName": "Shell", "result": "Exit code: 1\n\nCommand output:\n\n```\n\n```"}]}),
        ]
    }

    #[test]
    fn a_chat_reads_back_as_the_person_saw_it() {
        let dir = std::env::temp_dir().join(format!("keleuo-v1-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.db");
        let _ = std::fs::remove_file(&path);
        write_store(&path, &sample());
        let db = rusqlite::Connection::open(&path).unwrap();
        let events = V1.events(&db, "/tmp/p").unwrap();
        let chat = super::super::conversation("/tmp/p", "c1", events);
        let _ = std::fs::remove_dir_all(&dir);
        let mut seen = Vec::new();
        for item in &chat.items {
            match item {
                Item::User(t) => seen.push(format!("user: {t}")),
                Item::Assistant { text, .. } => seen.push(format!("reply: {}", text.lines().next().unwrap_or(""))),
                Item::Tool(t) => seen.push(format!("{} {} {}{}", t.name, t.summary, if t.is_error { "failed" } else { "ok" }, t.diff.as_ref().map_or(String::new(), |d| format!(" +{} -{}", d.added(), d.removed())))),
                _ => {}
            }
        }
        assert_eq!(
            seen,
            [
                "user: Read hello.txt, then create bye.txt containing the word bye, then run the shell command: ls",
                "reply: Reading `hello.txt`, creating `bye.txt`, then listing the directory.",
                "Read hello.txt ok",
                "Write bye.txt ok +1 -0",
                "Edit hello.txt ok +1 -1",
                "Shell ls ok",
                "reply: `hello.txt` contains `hello`.",
                "user: Run: false",
                "Shell false failed",
            ]
        );
        assert_eq!(chat.session_id.as_deref(), Some("c1"));
        assert!(!chat.busy);
    }
}
