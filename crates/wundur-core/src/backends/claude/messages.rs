//! Prompts Claude Code makes itself and records as `user` lines: a subagent's report or a
//! teammate's message (`<agent-message from="...">`, with the harness's framing around it) and
//! a background task's end (`<task-notification>`). Read here into what they say, so the chat
//! shows a report, not the wrapper.

use serde_json::Value;

/// Where the harness's preamble to a subagent's report ends.
const REPORT_FOLLOWS: &str = "The report follows:";

/// A prompt the interface made, not one the person typed.
pub fn is_harness_prompt(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with('<') || t.contains("<agent-message from=")
}

/// A `user` line that carries another agent's message: `(from, text)`.
pub fn agent_message(v: &Value) -> Option<(String, String)> {
    let origin = v.get("origin");
    if let Some(o) = origin.filter(|o| o.get("kind").and_then(Value::as_str) == Some("peer")) {
        let from = o.get("from").and_then(Value::as_str)?.to_string();
        let body = o.get("body").and_then(Value::as_str).map(str::to_string).or_else(|| wrapped(&line_text(v)?).map(|(_, b)| b))?;
        return Some((from, report_text(&body)));
    }
    let (from, body) = wrapped(&line_text(v)?)?;
    Some((from, report_text(&body)))
}

/// The line the interface records when the person stops a turn (no end-of-turn hook runs
/// then, and no answer follows).
pub fn is_interruption(v: &Value) -> bool {
    line_text(v).is_some_and(|t| t.trim_start().starts_with("[Request interrupted by user"))
}

/// A `<task-notification>`: `(the tool call that started the task, failed)`. A task stopped
/// from outside (its process ended) names only its own id (`<task-id>`); that is given then.
pub fn task_ended(v: &Value) -> Option<(String, bool)> {
    let text = line_text(v)?;
    let text = text.trim_start().strip_prefix("<task-notification>")?;
    let id = tag(text, "tool-use-id").or_else(|| tag(text, "task-id"))?;
    let status = tag(text, "status").unwrap_or_default();
    if matches!(status.as_str(), "running" | "") {
        return None;
    }
    Some((id, !matches!(status.as_str(), "completed" | "done")))
}

/// The text of a `user` line.
fn line_text(v: &Value) -> Option<String> {
    match v.get("message")?.get("content")? {
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) => Some(
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    }
}

/// `<agent-message from="X">body</agent-message>` anywhere in `text`: `(X, body)`.
fn wrapped(text: &str) -> Option<(String, String)> {
    let start = text.find("<agent-message from=\"")?;
    let rest = &text[start + "<agent-message from=\"".len()..];
    let quote = rest.find('"')?;
    let from = rest[..quote].to_string();
    let body = &rest[quote..];
    let body = &body[body.find('>')? + 1..];
    let body = body.find("</agent-message>").map_or(body, |end| &body[..end]);
    Some((from, body.to_string()))
}

/// The report itself: without the harness's preamble, and its lines out-dented (the harness
/// indents each by two spaces).
fn report_text(body: &str) -> String {
    let body = match body.find(REPORT_FOLLOWS) {
        Some(i) => &body[i + REPORT_FOLLOWS.len()..],
        None => body,
    };
    let body = body.strip_prefix('\n').unwrap_or(body);
    let lines: Vec<&str> = body.lines().collect();
    let indented = lines.iter().filter(|l| !l.trim().is_empty()).all(|l| l.starts_with("  "));
    let lines: Vec<&str> = if indented { lines.iter().map(|l| l.get(2..).unwrap_or("")).collect() } else { lines };
    lines.join("\n").trim().to_string()
}

fn tag(text: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&format!("</{name}>"))?;
    Some(text[start..start + end].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BODY: &str = "[Subagent hand-back] The text below is the final report of a subagent. The report follows:\n  **Recommendation:** keep it.\n  \n  - one\n  - two";

    #[test]
    fn reads_a_report_from_the_origin() {
        let v = json!({"type": "user", "isMeta": true, "origin": {"kind": "peer", "from": "a1", "body": BODY, "handback": true},
            "message": {"role": "user", "content": "ignored"}});
        let (from, text) = agent_message(&v).unwrap();
        assert_eq!(from, "a1");
        assert_eq!(text, "**Recommendation:** keep it.\n\n- one\n- two");
    }

    #[test]
    fn reads_a_report_from_the_wrapper() {
        let content = format!("Another Claude session sent a message:\n<agent-message from=\"a2\">\n{BODY}\n</agent-message>\n\nThat other session is an agent...");
        let v = json!({"type": "user", "message": {"role": "user", "content": content}});
        let (from, text) = agent_message(&v).unwrap();
        assert_eq!(from, "a2");
        assert!(text.starts_with("**Recommendation:**") && text.ends_with("- two"), "{text}");
    }

    #[test]
    fn plain_messages_are_not_reports() {
        let v = json!({"type": "user", "message": {"role": "user", "content": "hello"}});
        assert_eq!(agent_message(&v), None);
        assert!(!is_harness_prompt("hello"));
        assert!(is_harness_prompt("Another Claude session sent a message:\n<agent-message from=\"x\">"));
        assert!(is_harness_prompt("<task-notification>\n</task-notification>"));
    }

    #[test]
    fn reads_a_task_end() {
        let content = "<task-notification>\n<task-id>a1</task-id>\n<tool-use-id>toolu_1</tool-use-id>\n<status>completed</status>\n</task-notification>";
        let v = json!({"type": "user", "message": {"role": "user", "content": content}});
        assert_eq!(task_ended(&v), Some(("toolu_1".into(), false)));
        let failed = content.replace("completed", "failed");
        let v = json!({"type": "user", "message": {"role": "user", "content": failed}});
        assert_eq!(task_ended(&v), Some(("toolu_1".into(), true)));
    }
}
