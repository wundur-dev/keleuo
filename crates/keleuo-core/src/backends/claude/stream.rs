//! The Claude Code `stream-json` protocol: one JSON object per line on stdout, and the lines
//! Keleuo writes to stdin.
//!
//! Run as `claude -p --output-format stream-json --input-format stream-json --verbose
//! --include-partial-messages --permission-prompt-tool stdio`. What arrives (see the fixtures in
//! `tests/fixtures`):
//! - `system` / `init`: session id, model, permission mode, tools;
//! - `stream_event`: raw API stream events (`content_block_start`, `content_block_delta` with
//!   `text_delta`, ...), only with `--include-partial-messages`;
//! - `assistant`: one complete content block (text, thinking or tool_use) at a time;
//! - `user`: tool results, with the structured `tool_use_result` beside them;
//! - `control_request` / `can_use_tool`: a permission prompt, answered with a `control_response`;
//! - `result`: the end of a turn, with cost and usage.
//!
//! Unknown lines parse as [`Event::Other`] so a newer `claude` does not break the window.

use serde_json::{Value, json};

/// A content block of an assistant message.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Text(String),
    Thinking(String),
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

/// One parsed line of output.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Init {
        session_id: String,
        model: String,
        permission_mode: String,
        cwd: String,
    },
    /// `system` / `status` and similar progress notes.
    Status(String),
    /// A streamed content block begins: `kind` is `text`, `thinking` or `tool_use`.
    BlockStart {
        index: usize,
        kind: String,
        tool_name: Option<String>,
        tool_id: Option<String>,
    },
    /// A piece of streamed assistant text.
    TextDelta { index: usize, text: String },
    /// A complete assistant content block (or several).
    Assistant {
        message_id: String,
        model: String,
        blocks: Vec<Block>,
        parent_tool_use_id: Option<String>,
        /// The tokens this message's request sent (input, cached or not): how full the
        /// context was. A turn's `result` adds up every request, which is no measure of that.
        context_tokens: Option<u64>,
    },
    /// The result of a tool call.
    ToolResult {
        tool_use_id: String,
        /// The text Claude sees.
        content: String,
        is_error: bool,
        /// Claude Code's structured version (`tool_use_result`), when there is one.
        structured: Option<Value>,
        /// The subagent's Agent or Task tool call, when a subagent made the call.
        parent_tool_use_id: Option<String>,
        /// Images in the result (a Read of an image file), stored in the session's image folder.
        images: Vec<std::path::PathBuf>,
    },
    /// A user message echoed back (with `--replay-user-messages`) or sent by Claude Code.
    UserText(String),
    /// Claude asks before using a tool.
    PermissionRequest {
        request_id: String,
        tool_name: String,
        input: Value,
        description: Option<String>,
        tool_use_id: Option<String>,
    },
    /// The end of a turn.
    Result {
        is_error: bool,
        subtype: String,
        text: Option<String>,
        /// Cumulative for the process, in US dollars.
        total_cost_usd: Option<f64>,
        duration_ms: Option<u64>,
        num_turns: Option<u64>,
        input_tokens: u64,
        output_tokens: u64,
        session_id: String,
        /// The largest context window among the models the process used (`modelUsage`).
        context_window: Option<u64>,
    },
    /// A subagent (Agent or Task tool call) started.
    TaskStarted {
        tool_use_id: String,
        task_id: String,
        description: String,
        subagent_type: String,
    },
    /// A subagent's progress: what it is doing and how much it used so far.
    TaskProgress {
        tool_use_id: String,
        task_id: String,
        description: String,
        last_tool: Option<String>,
        total_tokens: Option<u64>,
        tool_uses: Option<u64>,
        duration_ms: Option<u64>,
    },
    /// A subagent finished (`completed`, `failed`, ...).
    TaskDone {
        tool_use_id: Option<String>,
        task_id: String,
        status: String,
        summary: Option<String>,
        total_tokens: Option<u64>,
        duration_ms: Option<u64>,
    },
    /// A line that is not JSON (Claude Code sometimes prints warnings on stdout).
    Unparsed(String),
    /// Valid JSON of a kind Keleuo does not show.
    Other(String),
}

/// Parse one line of output. Empty lines give `None`.
pub fn parse_line(line: &str) -> Option<Event> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Some(Event::Unparsed(line.to_string()));
    };
    Some(parse_value(&v))
}

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub fn parse_value(v: &Value) -> Event {
    let ty = v.get("type").and_then(Value::as_str).unwrap_or_default();
    match ty {
        "system" => match v.get("subtype").and_then(Value::as_str).unwrap_or_default() {
            "init" => Event::Init {
                session_id: s(v, "session_id"),
                model: s(v, "model"),
                permission_mode: s(v, "permissionMode"),
                cwd: s(v, "cwd"),
            },
            "status" => Event::Status(s(v, "status")),
            "task_started" => Event::TaskStarted {
                tool_use_id: s(v, "tool_use_id"),
                task_id: s(v, "task_id"),
                description: s(v, "description"),
                subagent_type: s(v, "subagent_type"),
            },
            "task_progress" => {
                let usage = v.get("usage").unwrap_or(&Value::Null);
                Event::TaskProgress {
                    tool_use_id: s(v, "tool_use_id"),
                    task_id: s(v, "task_id"),
                    description: s(v, "description"),
                    last_tool: v
                        .get("last_tool_name")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    total_tokens: usage.get("total_tokens").and_then(Value::as_u64),
                    tool_uses: usage.get("tool_uses").and_then(Value::as_u64),
                    duration_ms: usage.get("duration_ms").and_then(Value::as_u64),
                }
            }
            "task_notification" => {
                let usage = v.get("usage").unwrap_or(&Value::Null);
                Event::TaskDone {
                    tool_use_id: v
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    task_id: s(v, "task_id"),
                    status: s(v, "status"),
                    summary: v.get("summary").and_then(Value::as_str).map(str::to_string),
                    total_tokens: usage.get("total_tokens").and_then(Value::as_u64),
                    duration_ms: usage.get("duration_ms").and_then(Value::as_u64),
                }
            }
            "task_updated" => match v
                .get("patch")
                .and_then(|p| p.get("status"))
                .and_then(Value::as_str)
            {
                Some(status) if status != "running" => Event::TaskDone {
                    tool_use_id: None,
                    task_id: s(v, "task_id"),
                    status: status.to_string(),
                    summary: None,
                    total_tokens: None,
                    duration_ms: None,
                },
                _ => Event::Other("system/task_updated".into()),
            },
            other => Event::Other(format!("system/{other}")),
        },
        "stream_event" => parse_stream_event(v.get("event").unwrap_or(&Value::Null)),
        "assistant" => {
            let message = v.get("message").unwrap_or(&Value::Null);
            let blocks = message
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(parse_block)
                .collect();
            let usage = message.get("usage");
            let n = |k: &str| usage.and_then(|u| u.get(k)).and_then(Value::as_u64).unwrap_or(0);
            Event::Assistant {
                message_id: s(message, "id"),
                model: s(message, "model"),
                blocks,
                parent_tool_use_id: v
                    .get("parent_tool_use_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                context_tokens: usage.map(|_| n("input_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens")),
            }
        }
        "user" => parse_user(v),
        "control_request" => {
            let request = v.get("request").unwrap_or(&Value::Null);
            match request.get("subtype").and_then(Value::as_str) {
                Some("can_use_tool") => Event::PermissionRequest {
                    request_id: s(v, "request_id"),
                    tool_name: s(request, "tool_name"),
                    input: request.get("input").cloned().unwrap_or(Value::Null),
                    description: request
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    tool_use_id: request
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                },
                other => Event::Other(format!("control_request/{}", other.unwrap_or_default())),
            }
        }
        "result" => {
            let usage = v.get("usage").unwrap_or(&Value::Null);
            let tokens = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
            Event::Result {
                is_error: v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                subtype: s(v, "subtype"),
                text: v.get("result").and_then(Value::as_str).map(str::to_string),
                total_cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
                duration_ms: v.get("duration_ms").and_then(Value::as_u64),
                num_turns: v.get("num_turns").and_then(Value::as_u64),
                input_tokens: tokens("input_tokens")
                    + tokens("cache_read_input_tokens")
                    + tokens("cache_creation_input_tokens"),
                output_tokens: tokens("output_tokens"),
                session_id: s(v, "session_id"),
                // Subagents may run a smaller model; the session's own is the largest window.
                context_window: v
                    .get("modelUsage")
                    .and_then(Value::as_object)
                    .and_then(|m| m.values().filter_map(|u| u.get("contextWindow")?.as_u64()).filter(|n| *n > 0).max()),
            }
        }
        other => Event::Other(other.to_string()),
    }
}

fn parse_stream_event(e: &Value) -> Event {
    let index = e.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
    match e.get("type").and_then(Value::as_str).unwrap_or_default() {
        "content_block_start" => {
            let block = e.get("content_block").unwrap_or(&Value::Null);
            Event::BlockStart {
                index,
                kind: s(block, "type"),
                tool_name: block
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                tool_id: block.get("id").and_then(Value::as_str).map(str::to_string),
            }
        }
        "content_block_delta" => {
            let delta = e.get("delta").unwrap_or(&Value::Null);
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => Event::TextDelta {
                    index,
                    text: s(delta, "text"),
                },
                other => Event::Other(format!("delta/{}", other.unwrap_or_default())),
            }
        }
        other => Event::Other(format!("stream/{other}")),
    }
}

fn parse_block(b: &Value) -> Option<Block> {
    match b.get("type")?.as_str()? {
        "text" => Some(Block::Text(s(b, "text"))),
        "thinking" => Some(Block::Thinking(s(b, "thinking"))),
        "tool_use" => Some(Block::ToolUse {
            id: s(b, "id"),
            name: s(b, "name"),
            input: b.get("input").cloned().unwrap_or(Value::Null),
        }),
        _ => None,
    }
}

fn parse_user(v: &Value) -> Event {
    let content = v.get("message").and_then(|m| m.get("content"));
    if let Some(text) = content.and_then(Value::as_str) {
        return Event::UserText(text.to_string());
    }
    let blocks = content
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for block in &blocks {
        if block.get("type").and_then(Value::as_str) == Some("tool_result") {
            return Event::ToolResult {
                tool_use_id: s(block, "tool_use_id"),
                content: tool_result_text(block.get("content").unwrap_or(&Value::Null)),
                is_error: block
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                structured: v
                    .get("tool_use_result")
                    .or_else(|| v.get("toolUseResult"))
                    .filter(|r| r.is_object())
                    .cloned(),
                parent_tool_use_id: v
                    .get("parent_tool_use_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                images: result_images(v, block.get("content").unwrap_or(&Value::Null)),
            };
        }
    }
    let text: Vec<String> = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .map(|b| s(b, "text"))
        .collect();
    if text.is_empty() {
        Event::Other("user".into())
    } else {
        Event::UserText(text.join("\n"))
    }
}

/// The images in a tool result's content, stored in the session's image folder (named by
/// their content, so a result read again is stored once).
fn result_images(line: &Value, content: &Value) -> Vec<std::path::PathBuf> {
    let Some(items) = content.as_array() else { return Vec::new() };
    if !items.iter().any(|i| i.get("type").and_then(Value::as_str) == Some("image")) {
        return Vec::new();
    }
    let session = line.get("session_id").or_else(|| line.get("sessionId")).and_then(Value::as_str);
    let Some(dir) = crate::images::session_dir(session) else { return Vec::new() };
    items
        .iter()
        .filter(|i| i.get("type").and_then(Value::as_str) == Some("image"))
        .filter_map(|i| {
            let source = i.get("source")?;
            let data = crate::images::base64(source.get("data")?.as_str()?)?;
            let ext = crate::images::extension(source.get("media_type").and_then(Value::as_str).unwrap_or("image/png"));
            crate::images::store(&dir, &data, ext).ok()
        })
        .collect()
}

/// A tool result's content is a string or a list of `{type: text, text}` blocks.
fn tool_result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|i| match i.get("type").and_then(Value::as_str) {
                Some("text") => s(i, "text"),
                Some(other) => format!("[{other}]"),
                None => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The stdin line for a new user turn.
pub fn user_message(text: &str) -> String {
    json!({"type": "user", "message": {"role": "user", "content": text}}).to_string()
}

/// The stdin line for a user turn with images: each read from its file (on this machine) and
/// embedded, then the text.
pub fn user_message_with_images(text: &str, images: &[std::path::PathBuf]) -> std::io::Result<String> {
    use base64::Engine as _;
    let mut content = Vec::new();
    for path in images {
        let data = base64::engine::general_purpose::STANDARD.encode(std::fs::read(path)?);
        content.push(json!({"type": "image", "source": {"type": "base64", "media_type": image_type(path), "data": data}}));
    }
    if !text.is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    Ok(json!({"type": "user", "message": {"role": "user", "content": content}}).to_string())
}

/// An image's media type, from its file name (PNG unless it says otherwise).
fn image_type(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "image/png",
    }
}

/// The stdin line answering a permission prompt. `input` is the tool input to run with
/// (normally the one in the request).
pub fn permission_response(request_id: &str, allow: bool, input: &Value) -> String {
    let response = if allow {
        json!({"behavior": "allow", "updatedInput": input})
    } else {
        json!({"behavior": "deny", "message": "The user declined this in Keleuo."})
    };
    json!({"type": "control_response", "response": {"subtype": "success", "request_id": request_id, "response": response}})
        .to_string()
}

/// The stdin line that stops the current turn.
pub fn interrupt_request(request_id: &str) -> String {
    json!({"type": "control_request", "request_id": request_id, "request": {"subtype": "interrupt"}}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_and_unknown_lines() {
        assert_eq!(parse_line(""), None);
        assert_eq!(
            parse_line("warning: something"),
            Some(Event::Unparsed("warning: something".into()))
        );
        assert_eq!(
            parse_line(r#"{"type":"brand_new"}"#),
            Some(Event::Other("brand_new".into()))
        );
    }

    #[test]
    fn permission_response_shapes() {
        let allow: Value =
            serde_json::from_str(&permission_response("r1", true, &json!({"command": "ls"})))
                .unwrap();
        assert_eq!(allow["response"]["request_id"], "r1");
        assert_eq!(allow["response"]["response"]["behavior"], "allow");
        assert_eq!(
            allow["response"]["response"]["updatedInput"]["command"],
            "ls"
        );
        let deny: Value =
            serde_json::from_str(&permission_response("r2", false, &Value::Null)).unwrap();
        assert_eq!(deny["response"]["response"]["behavior"], "deny");
    }

    #[test]
    fn user_message_shape() {
        let v: Value = serde_json::from_str(&user_message("hi")).unwrap();
        assert_eq!(v["message"]["content"], "hi");
        assert_eq!(v["type"], "user");
    }

    #[test]
    fn tool_result_with_block_list() {
        let line = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":[{"type":"text","text":"a"},{"type":"image"}]}]}}"#;
        match parse_line(line).unwrap() {
            Event::ToolResult {
                content,
                is_error,
                structured,
                ..
            } => {
                assert_eq!(content, "a\n[image]");
                assert!(!is_error);
                assert_eq!(structured, None);
            }
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn images_in_a_tool_result_are_kept() {
        let session = format!("keleuo-test-tool-images-{}", std::process::id());
        let line = format!(
            r#"{{"type":"user","session_id":"{session}","message":{{"content":[{{"type":"tool_result","tool_use_id":"t","content":[{{"type":"image","source":{{"type":"base64","media_type":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg=="}}}}]}}]}}}}"#
        );
        match parse_line(&line).unwrap() {
            Event::ToolResult { images, content, .. } => {
                assert_eq!(content, "[image]");
                assert_eq!(images.len(), 1);
                assert!(images[0].exists() && images[0].extension().is_some_and(|e| e == "png"));
                if let Some(dir) = images[0].parent() {
                    let _ = std::fs::remove_dir_all(dir);
                }
            }
            e => panic!("{e:?}"),
        }
    }
}
