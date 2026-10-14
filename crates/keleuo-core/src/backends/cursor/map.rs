//! Cursor Agent's `stream-json` output (`cursor-agent -p --output-format stream-json
//! --stream-partial-output`) onto the neutral [`crate::agent::Event`]s.
//!
//! One run is one turn: `system/init`, the person's message back, `thinking` deltas, the reply
//! as `assistant` deltas (they carry `timestamp_ms`) closed by the whole block (no
//! `timestamp_ms`), `tool_call` `started` / `completed` pairs (`tool_call.<kind>ToolCall` with
//! `args`, then `result.success`, `result.rejected` or `result.error`), and a `result`.

use std::path::PathBuf;

use serde_json::Value;

use crate::agent::Event;
use crate::diff::{DiffLine, FileDiff, Hunk, LineKind};

/// Keeps what the mapping needs between lines of one run.
#[derive(Default)]
pub struct Mapper {
    /// The folders a path may start with (Keleuo's and the one Cursor reports), for short paths.
    roots: Vec<String>,
    /// The mode the run was started in, reported as the session's.
    mode: Option<String>,
    /// A reply block is being streamed.
    text_open: bool,
    /// "thinking" was reported for the current stretch of thinking.
    thinking: bool,
    /// The run reported its end (`result`).
    pub ended: bool,
}

impl Mapper {
    pub fn new(cwd: impl Into<String>, mode: Option<String>) -> Self {
        Self { roots: vec![cwd.into()], mode, ..Default::default() }
    }

    /// One line of output.
    pub fn map_line(&mut self, line: &str) -> Vec<Event> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return vec![Event::Notice(line.to_string())];
        };
        self.map(&v)
    }

    pub fn map(&mut self, v: &Value) -> Vec<Event> {
        match (s(v, "type"), s(v, "subtype")) {
            ("system", "init") => {
                let cwd = s(v, "cwd").to_string();
                if !cwd.is_empty() && !self.roots.contains(&cwd) {
                    self.roots.push(cwd.clone());
                }
                vec![Event::Started {
                    session_id: some(s(v, "session_id")),
                    model: some(s(v, "model")),
                    permission_mode: self.mode.clone(),
                    cwd: some(&cwd),
                }]
            }
            ("thinking", "delta") if !self.thinking => {
                self.thinking = true;
                vec![Event::Status("thinking".into())]
            }
            ("thinking", _) => Vec::new(),
            ("assistant", _) => {
                self.thinking = false;
                let text = assistant_text(v);
                if v.get("timestamp_ms").is_some() {
                    // A piece of the reply as it is written.
                    let mut out = Vec::new();
                    if !self.text_open {
                        self.text_open = true;
                        out.push(Event::TextStart);
                    }
                    out.push(Event::TextDelta(text));
                    out
                } else {
                    // The whole block, in place of the pieces.
                    self.text_open = false;
                    vec![Event::Text { text, parent: None }]
                }
            }
            ("tool_call", phase) => {
                self.thinking = false;
                self.text_open = false;
                let id = s(v, "call_id").to_string();
                let Some((kind, call)) = tool(v) else { return Vec::new() };
                let args = call.get("args").cloned().unwrap_or(Value::Null);
                if phase == "started" {
                    let summary = self.summary(kind, &args, call);
                    vec![Event::ToolCall { id, name: tool_name(kind), summary, input: args, diff: None, parent: None }]
                } else {
                    vec![self.result(id, kind, &args, call.get("result").unwrap_or(&Value::Null))]
                }
            }
            ("result", _) => {
                self.ended = true;
                self.text_open = false;
                let usage = v.get("usage").unwrap_or(&Value::Null);
                let n = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
                let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                vec![Event::TurnEnded {
                    error: is_error.then(|| some(s(v, "result")).unwrap_or_else(|| s(v, "subtype").to_string())),
                    cost_total_usd: None,
                    input_tokens: n("inputTokens") + n("cacheReadTokens") + n("cacheWriteTokens"),
                    output_tokens: n("outputTokens"),
                    duration_ms: v.get("duration_ms").and_then(Value::as_u64),
                    session_id: some(s(v, "session_id")),
                }]
            }
            _ => Vec::new(),
        }
    }

    /// A path as it is shown: inside the folder, from there.
    fn short(&self, path: &str) -> String {
        self.roots
            .iter()
            .filter(|r| !r.is_empty())
            .find_map(|r| path.strip_prefix(r.as_str())?.strip_prefix('/'))
            .unwrap_or(path)
            .to_string()
    }

    /// What the tool card says: the file, the command or the pattern.
    fn summary(&self, kind: &str, args: &Value, call: &Value) -> String {
        if kind == "shell" {
            return s(args, "command").to_string();
        }
        for key in ["path", "targetFile", "pattern", "globPattern", "query", "url"] {
            let value = s(args, key);
            if !value.is_empty() {
                return if key == "path" || key == "targetFile" { self.short(value) } else { value.to_string() };
            }
        }
        s(call, "description").to_string()
    }

    fn result(&self, id: String, kind: &str, args: &Value, result: &Value) -> Event {
        let path = some(s(args, "path")).map(PathBuf::from);
        let changes_file = matches!(kind, "edit" | "write" | "delete");
        let (output, is_error, diff) = if let Some(ok) = result.get("success") {
            match kind {
                "shell" => {
                    let out = some(s(ok, "interleavedOutput"))
                        .unwrap_or_else(|| format!("{}{}", s(ok, "stdout"), s(ok, "stderr")));
                    let code = ok.get("exitCode").and_then(Value::as_i64).unwrap_or(0);
                    (out, code != 0, None)
                }
                "read" => (s(ok, "content").to_string(), false, None),
                _ => {
                    let diff = path.as_ref().and_then(|p| parse_diff(p.clone(), s(ok, "diffString")));
                    let out = some(s(ok, "message")).unwrap_or_else(|| compact(ok));
                    (out, false, diff)
                }
            }
        } else if let Some(no) = result.get("rejected") {
            let reason = some(s(no, "reason")).unwrap_or_else(|| "not allowed by Cursor's permissions (its allowlist)".into());
            (format!("Rejected: {reason}"), true, None)
        } else if let Some(err) = result.get("error") {
            let msg = ["errorMessage", "message", "error"].iter().find_map(|k| some(s(err, k))).unwrap_or_else(|| compact(err));
            (msg, true, None)
        } else {
            (compact(result), false, None)
        };
        let file_changed = (changes_file && !is_error).then_some(path).flatten();
        Event::ToolResult { id, output, is_error, diff, file_changed, parent: None }
    }
}

/// The tool call's kind (`read` for `readToolCall`) and its body.
fn tool(v: &Value) -> Option<(&str, &Value)> {
    v.get("tool_call")?
        .as_object()?
        .iter()
        .find_map(|(k, body)| Some((k.strip_suffix("ToolCall")?, body)))
}

/// A tool's name on its card: `read` → `Read`, `shell` → `Shell`.
fn tool_name(kind: &str) -> String {
    let mut chars = kind.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn assistant_text(v: &Value) -> String {
    v.pointer("/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|b| s(b, "type") == "text")
        .map(|b| s(b, "text"))
        .collect()
}

/// A unified diff (`--- a`, `+++ b`, `@@ -1,2 +1,3 @@`, lines) as a [`FileDiff`].
pub fn parse_diff(path: PathBuf, text: &str) -> Option<FileDiff> {
    let mut created = false;
    let mut hunks: Vec<Hunk> = Vec::new();
    for line in text.lines() {
        if let Some(from) = line.strip_prefix("--- ") {
            created = from.trim() == "/dev/null";
        } else if line.starts_with("+++ ") {
        } else if let Some(head) = line.strip_prefix("@@ ") {
            let mut parts = head.split_whitespace();
            let start = |p: Option<&str>, sign: char| {
                p.and_then(|p| p.strip_prefix(sign)).and_then(|p| p.split(',').next()?.parse::<usize>().ok()).unwrap_or(0)
            };
            let old_start = start(parts.next(), '-');
            let new_start = start(parts.next(), '+');
            hunks.push(Hunk { old_start, new_start, lines: Vec::new() });
        } else if let Some(hunk) = hunks.last_mut() {
            let (kind, rest) = match line.chars().next() {
                Some('+') => (LineKind::Added, &line[1..]),
                Some('-') => (LineKind::Removed, &line[1..]),
                Some(' ') => (LineKind::Context, &line[1..]),
                _ => continue,
            };
            hunk.lines.push(DiffLine { kind, text: rest.to_string() });
        }
    }
    (!hunks.is_empty()).then_some(FileDiff { path, created, hunks })
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn some(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_string())
}

/// A short JSON rendering of a result with no text of its own.
fn compact(v: &Value) -> String {
    let text = v.to_string();
    match text.char_indices().nth(2000) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text,
    }
}
