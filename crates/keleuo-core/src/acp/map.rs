//! What ACP standardises, onto the neutral [`crate::agent::Event`]s: `session/update`
//! notifications (the reply and thinking as they are written, tool calls and their updates,
//! the person's own messages when a session is loaded again), `session/request_permission`
//! requests, and the end of a prompt. Updates an agent adds of its own, and extension
//! methods, are its backend's to handle; [`Mapper::update`] leaves them alone.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::Reply;
use crate::agent::Event;
use crate::diff::{self, FileDiff};

/// Keeps what the mapping needs between messages of one session.
#[derive(Default)]
pub struct Mapper {
    /// A reply is being written.
    text_open: bool,
    /// "thinking" was said for the current stretch of thinking.
    thinking: bool,
    /// The person's message being replayed (a session loaded again), in pieces.
    user: String,
    /// Tool calls so far: their name, input and title, as last known.
    tools: HashMap<String, (String, Value, String)>,
}

impl Mapper {
    /// A `session/update`'s `update`. `None` for an update ACP does not standardise (or that
    /// Keleuo does not show), for the backend to look at.
    pub fn update(&mut self, update: &Value) -> Option<Vec<Event>> {
        let kind = s(update, "sessionUpdate");
        let mut out = Vec::new();
        if kind != "user_message_chunk" {
            self.flush_user(&mut out);
        }
        match kind {
            "agent_message_chunk" => {
                self.thinking = false;
                let text = content_text(update.get("content")?);
                if !self.text_open {
                    self.text_open = true;
                    out.push(Event::TextStart);
                }
                out.push(Event::TextDelta(text));
            }
            "agent_thought_chunk" => {
                if !self.thinking {
                    self.thinking = true;
                    out.push(Event::Status("thinking".into()));
                }
            }
            "user_message_chunk" => {
                self.user.push_str(&content_text(update.get("content")?));
            }
            "tool_call" => {
                self.text_open = false;
                self.thinking = false;
                let id = s(update, "toolCallId").to_string();
                let name = tool_name(update);
                let input = update.get("rawInput").cloned().unwrap_or(Value::Null);
                self.tools.insert(id.clone(), (name.clone(), input.clone(), s(update, "title").to_string()));
                out.push(Event::ToolCall { id: id.clone(), name, summary: s(update, "title").to_string(), input, diff: diff_of(update), parent: None });
                out.extend(self.finished(update));
            }
            "tool_call_update" => {
                self.text_open = false;
                let id = s(update, "toolCallId");
                // New details before it ends (its title, input or diff once known).
                if let Some((name, input, title)) = self.tools.get(id).cloned() {
                    let input = update.get("rawInput").cloned().unwrap_or(input);
                    let title = update.get("title").and_then(Value::as_str).map_or(title, str::to_string);
                    if update.get("title").is_some() || update.get("rawInput").is_some() || diff_of(update).is_some() {
                        out.push(Event::ToolCall { id: id.into(), name: name.clone(), summary: title.clone(), input: input.clone(), diff: diff_of(update), parent: None });
                    }
                    self.tools.insert(id.to_string(), (name, input, title));
                }
                out.extend(self.finished(update));
            }
            "plan" | "available_commands_update" | "current_mode_update" | "session_info_update" => return None,
            _ => return None,
        }
        Some(out)
    }

    /// The person's replayed message, once it is whole (the next update is something else).
    fn flush_user(&mut self, out: &mut Vec<Event>) {
        if !self.user.is_empty() {
            out.push(Event::UserText(std::mem::take(&mut self.user)));
            self.text_open = false;
        }
    }

    /// A tool call that has ended (completed or failed), as its result.
    fn finished(&self, update: &Value) -> Option<Event> {
        let status = s(update, "status");
        if !matches!(status, "completed" | "failed") {
            return None;
        }
        let id = s(update, "toolCallId").to_string();
        let is_error = status == "failed";
        let diff = diff_of(update);
        let edits = diff.is_some() || matches!(self.tools.get(&id).map(|(n, ..)| n.as_str()), Some("Edit" | "Write" | "Delete" | "Move"));
        let file_changed = (!is_error && edits)
            .then(|| diff.as_ref().map(|d| d.path.clone()).or_else(|| first_location(update)))
            .flatten();
        Some(Event::ToolResult { id, output: output_of(update), is_error, diff, file_changed, parent: None })
    }

    /// What is left at the end of a prompt (a replayed message with nothing after it).
    pub fn finish(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        self.flush_user(&mut out);
        self.text_open = false;
        self.thinking = false;
        out
    }
}

/// The end of a prompt, from `session/prompt`'s reply.
pub fn turn_ended(reply: &Reply, session_id: &str) -> Event {
    let stop = reply.as_ref().ok().map(|r| s(r, "stopReason").to_string()).unwrap_or_default();
    if stop == "cancelled" {
        return Event::Interrupted;
    }
    let error = match (reply, stop.as_str()) {
        (Err(e), _) => Some(e.to_string()),
        (Ok(_), "refusal") => Some("The agent refused to continue.".into()),
        (Ok(_), "max_tokens") => Some("The reply reached the most it may write.".into()),
        (Ok(_), "max_turn_requests") => Some("The turn reached the most steps it may take.".into()),
        _ => None,
    };
    Event::TurnEnded { error, cost_total_usd: None, input_tokens: 0, output_tokens: 0, duration_ms: None, session_id: Some(session_id.to_string()) }
}

/// A `session/request_permission` request as a permission prompt (`request_id` names the
/// request when answering; the options travel in the input, see [`permission_outcome`]).
pub fn permission(request_id: &str, params: &Value) -> Event {
    let call = params.get("toolCall").cloned().unwrap_or(Value::Null);
    let title = s(&call, "title").to_string();
    Event::Permission {
        request_id: request_id.to_string(),
        tool: tool_name(&call),
        summary: title,
        description: None,
        input: json!({"toolCall": call, "options": params.get("options").cloned().unwrap_or(json!([]))}),
    }
}

/// The answer to a permission request whose input (from [`permission`]) offers `options`:
/// the first allowing option ("once" before "always") or refusing one, or cancelled when
/// there is none.
pub fn permission_outcome(input: &Value, allow: bool) -> Value {
    let options = input.get("options").and_then(Value::as_array).cloned().unwrap_or_default();
    let wanted: &[&str] = if allow { &["allow_once", "allow_always"] } else { &["reject_once", "reject_always"] };
    let pick = wanted
        .iter()
        .find_map(|kind| options.iter().find(|o| s(o, "kind") == *kind))
        .and_then(|o| o.get("optionId").cloned());
    match pick {
        Some(id) => json!({"outcome": {"outcome": "selected", "optionId": id}}),
        None => json!({"outcome": {"outcome": "cancelled"}}),
    }
}

/// A tool call's name on its card, from its kind (`read` is Read, `execute` is Shell); an
/// unknown kind keeps the title.
fn tool_name(call: &Value) -> String {
    match s(call, "kind") {
        "read" => "Read",
        "edit" => "Edit",
        "delete" => "Delete",
        "move" => "Move",
        "search" => "Search",
        "execute" => "Shell",
        "think" => "Think",
        "fetch" => "Fetch",
        "switch_mode" => "Mode",
        _ => return if s(call, "title").is_empty() { "Tool".into() } else { s(call, "title").to_string() },
    }
    .to_string()
}

/// The first diff in a tool call's content.
fn diff_of(call: &Value) -> Option<FileDiff> {
    let d = call.get("content")?.as_array()?.iter().find(|c| s(c, "type") == "diff")?;
    let old = d.get("oldText").and_then(Value::as_str);
    Some(FileDiff {
        path: PathBuf::from(s(d, "path")),
        created: old.is_none(),
        hunks: vec![diff::text_hunk(old.unwrap_or(""), s(d, "newText"))],
    })
}

/// What a tool call returned: the text of its content, else its raw output.
fn output_of(call: &Value) -> String {
    let text: Vec<String> = call
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|c| s(c, "type") == "content")
        .filter_map(|c| c.get("content").map(content_text))
        .collect();
    if !text.is_empty() {
        return text.join("\n");
    }
    match call.get("rawOutput") {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Null) | None => String::new(),
        Some(v) => v.to_string(),
    }
}

fn first_location(call: &Value) -> Option<PathBuf> {
    let path = s(call.get("locations")?.as_array()?.first()?, "path");
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// A content block's text ("" for an image and the like).
fn content_text(block: &Value) -> String {
    match s(block, "type") {
        "text" => s(block, "text").to_string(),
        "resource_link" => s(block, "uri").to_string(),
        _ => String::new(),
    }
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{Conversation, Item};

    #[test]
    fn a_turn_reads_as_a_chat() {
        let mut m = Mapper::default();
        let mut chat = Conversation::new("/tmp/p");
        chat.push_user("make bye.txt");
        let updates = [
            (json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "hmm"}})),
            (json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Writing "}})),
            (json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "it."}})),
            (json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Write bye.txt", "kind": "edit", "status": "pending",
                "rawInput": {"path": "/tmp/p/bye.txt"}})),
            (json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed",
                "content": [{"type": "diff", "path": "/tmp/p/bye.txt", "oldText": null, "newText": "bye\n"}]})),
            (json!({"sessionUpdate": "tool_call", "toolCallId": "t2", "title": "`ls`", "kind": "execute", "status": "in_progress"})),
            (json!({"sessionUpdate": "tool_call_update", "toolCallId": "t2", "status": "failed",
                "content": [{"type": "content", "content": {"type": "text", "text": "ls: no such dir"}}]})),
            (json!({"sessionUpdate": "plan", "entries": []})),
        ];
        for u in &updates {
            for e in m.update(u).unwrap_or_default() {
                chat.apply(e);
            }
        }
        chat.apply(turn_ended(&Ok(json!({"stopReason": "end_turn"})), "s1"));
        let shown: Vec<String> = chat
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Assistant { text, .. } => Some(format!("reply: {text}")),
                Item::Tool(t) => Some(format!("{} {} {}{}", t.name, t.summary, if t.is_error { "failed" } else { "ok" }, t.diff.as_ref().map_or(String::new(), |d| format!(" +{}", d.added())))),
                _ => None,
            })
            .collect();
        assert_eq!(shown, ["reply: Writing it.", "Edit Write bye.txt ok +1", "Shell `ls` failed"]);
        assert!(!chat.busy);
        assert!(m.update(&json!({"sessionUpdate": "plan", "entries": []})).is_none());
    }

    #[test]
    fn a_loaded_session_replays_the_persons_messages() {
        let mut m = Mapper::default();
        let mut events = Vec::new();
        for u in [
            json!({"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": "hello "}}),
            json!({"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": "there"}}),
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Hi."}}),
            json!({"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": "bye"}}),
        ] {
            events.extend(m.update(&u).unwrap());
        }
        events.extend(m.finish());
        assert_eq!(events[0], Event::UserText("hello there".into()));
        assert_eq!(events.last(), Some(&Event::UserText("bye".into())));
    }

    #[test]
    fn permissions_are_answered_with_the_agents_own_options() {
        let params = json!({"sessionId": "s", "toolCall": {"toolCallId": "t", "title": "`touch made.txt`", "kind": "execute"},
            "options": [{"optionId": "a", "name": "Allow", "kind": "allow_once"}, {"optionId": "aa", "name": "Always", "kind": "allow_always"},
                        {"optionId": "r", "name": "Reject", "kind": "reject_once"}]});
        let Event::Permission { tool, summary, input, .. } = permission("7", &params) else { panic!() };
        assert_eq!((tool.as_str(), summary.as_str()), ("Shell", "`touch made.txt`"));
        assert_eq!(permission_outcome(&input, true), json!({"outcome": {"outcome": "selected", "optionId": "a"}}));
        assert_eq!(permission_outcome(&input, false), json!({"outcome": {"outcome": "selected", "optionId": "r"}}));
        assert_eq!(permission_outcome(&json!({}), true), json!({"outcome": {"outcome": "cancelled"}}));
    }

    #[test]
    fn a_cancelled_prompt_is_an_interruption() {
        assert_eq!(turn_ended(&Ok(json!({"stopReason": "cancelled"})), "s"), Event::Interrupted);
        assert!(matches!(turn_ended(&Ok(json!({"stopReason": "refusal"})), "s"), Event::TurnEnded { error: Some(_), .. }));
    }
}
