//! Claude Code's hooks, as the window uses them while Claude Code's own interface runs a
//! session: a settings file passed with `--settings` makes the interface run the window's hook
//! command on its events (nothing in the person's own settings changes), and each call is read
//! back as a neutral [`Hook`]. A permission request waits for the window's answer.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::summary::tool_summary;
use crate::agent::Hook;

/// The events the window listens to, and whether they match tools.
const EVENTS: [(&str, bool); 10] = [
    ("SessionStart", false),
    ("UserPromptSubmit", false),
    ("PreToolUse", true),
    ("PermissionRequest", true),
    ("PostToolUse", true),
    ("PostToolUseFailure", true),
    ("PermissionDenied", true),
    ("Stop", false),
    ("StopFailure", false),
    ("Notification", false),
];

/// A permission request may wait this long for an answer (seconds); the terminal interface
/// asks at the same time, so an answer there ends the wait for the interface.
const PERMISSION_TIMEOUT: u64 = 3600;

/// The settings file: every event in [`EVENTS`] runs `command(event)`.
pub fn settings(command: &dyn Fn(&str) -> String) -> String {
    let mut hooks = serde_json::Map::new();
    for (event, tools) in EVENTS {
        let mut hook = json!({"type": "command", "command": command(event)});
        hook["timeout"] = json!(if event == "PermissionRequest" { PERMISSION_TIMEOUT } else { 10 });
        let mut group = json!({"hooks": [hook]});
        if tools {
            group["matcher"] = json!("*");
        }
        hooks.insert(event.to_string(), json!([group]));
    }
    serde_json::to_string_pretty(&json!({"hooks": hooks})).unwrap_or_default()
}

/// One hook call in neutral terms.
pub fn parse(event: &str, payload: &Value, cwd: &str) -> Option<Hook> {
    let s = |k: &str| payload.get(k).and_then(Value::as_str).map(str::to_string);
    let tool = || s("tool_name").unwrap_or_default();
    let input = || payload.get("tool_input").cloned().unwrap_or(Value::Null);
    Some(match event {
        "SessionStart" => Hook::Started { session_id: s("session_id"), transcript: s("transcript_path").map(PathBuf::from) },
        "UserPromptSubmit" => Hook::Prompt(s("prompt").unwrap_or_default()),
        "PreToolUse" => Hook::ToolStarting { id: s("tool_use_id"), tool: tool(), input: input() },
        "PermissionRequest" => {
            let input = input();
            Hook::Permission {
                summary: tool_summary(&tool(), &input, cwd),
                description: input.get("description").and_then(Value::as_str).map(str::to_string),
                tool: tool(),
                input,
            }
        }
        "PostToolUse" | "PostToolUseFailure" | "PermissionDenied" => {
            Hook::ToolFinished { id: s("tool_use_id"), tool: tool(), input: input() }
        }
        "Stop" | "StopFailure" => Hook::TurnEnded,
        "Notification" => Hook::Attention(s("message").unwrap_or_default()),
        _ => return None,
    })
}

/// A permission hook's answer.
pub fn reply(allow: bool) -> String {
    let decision = if allow {
        json!({"behavior": "allow"})
    } else {
        json!({"behavior": "deny", "message": "Denied in Keleion."})
    };
    json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": decision}}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_settings_name_every_event() {
        let text = settings(&|e| format!("hook {e}"));
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["hooks"]["PermissionRequest"][0]["hooks"][0]["command"], "hook PermissionRequest");
        assert_eq!(v["hooks"]["PermissionRequest"][0]["matcher"], "*");
        assert!(v["hooks"]["Stop"][0].get("matcher").is_none());
    }

    #[test]
    fn hook_calls_read_as_neutral_hooks() {
        // Recorded from Claude Code 2.1.296 (paths shortened).
        let perm = json!({"session_id": "s1", "hook_event_name": "PermissionRequest", "tool_name": "Bash",
            "tool_input": {"command": "curl -sI https://example.com", "description": "Fetch the status"},
            "permission_suggestions": []});
        match parse("PermissionRequest", &perm, "/p").unwrap() {
            Hook::Permission { tool, description, .. } => {
                assert_eq!(tool, "Bash");
                assert_eq!(description.as_deref(), Some("Fetch the status"));
            }
            other => panic!("{other:?}"),
        }
        let start = json!({"session_id": "s1", "transcript_path": "/h/.claude/projects/p/s1.jsonl", "source": "startup"});
        assert_eq!(
            parse("SessionStart", &start, "/p"),
            Some(Hook::Started { session_id: Some("s1".into()), transcript: Some("/h/.claude/projects/p/s1.jsonl".into()) })
        );
        assert_eq!(parse("Stop", &json!({}), "/p"), Some(Hook::TurnEnded));
        let reply: Value = serde_json::from_str(&reply(false)).unwrap();
        assert_eq!(reply["hookSpecificOutput"]["decision"]["behavior"], "deny");
    }
}
