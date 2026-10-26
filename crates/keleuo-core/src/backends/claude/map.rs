//! Claude Code's `stream-json` events onto the neutral [`crate::agent::Event`]s. Claude's
//! tool names live here: Edit, MultiEdit and Write give diffs, Agent and Task start subagents.

use std::collections::HashMap;

use serde_json::Value;

use super::stream::{Block, Event as Raw};
use super::summary::tool_summary;
use crate::agent::Event;
use crate::diff;

/// Is this a tool that starts subagents? A workflow (Claude Code's Workflow tool: a script
/// running many agents in the background) shows as one, running until its task ends.
pub fn is_agent_tool(name: &str) -> bool {
    matches!(name, "Agent" | "Task" | WORKFLOW)
}

/// The tool that runs a workflow.
pub const WORKFLOW: &str = "Workflow";

/// Keeps what the mapping needs between events: the session folder (for short paths), each
/// tool call's name and input (for the diff of its result), task ids to tool calls.
#[derive(Default)]
pub struct Mapper {
    /// When the usage limit's window starts again, as the last limit event said.
    limit_resets: Option<u64>,
    cwd: String,
    tools: HashMap<String, (String, Value)>,
    tasks: HashMap<String, String>,
    /// Replaying a session file: the person's messages come through as text.
    pub history: bool,
    /// How full the context was at the turn's last request (see `Raw::Assistant`).
    context: Option<u64>,
}

impl Mapper {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self { cwd: cwd.into(), ..Default::default() }
    }

    /// Whether a tool call seen so far started a subagent.
    pub fn started_agent(&self, tool_use_id: &str) -> bool {
        self.tools.get(tool_use_id).is_some_and(|(name, _)| is_agent_tool(name))
    }

    /// The tool call that started subagent `id`: `id` itself, or the call that launched the
    /// background agent with that id. `None` when `id` is no subagent of this session.
    pub fn agent_call(&self, id: &str) -> Option<String> {
        if self.started_agent(id) {
            return Some(id.to_string());
        }
        self.tasks.get(id).filter(|call| self.started_agent(call)).cloned()
    }

    /// Whether a tool call seen so far is one that does not start a subagent. Claude Code
    /// reports background commands (`task_type: local_bash`) as tasks too; they are not agents.
    fn other_tool(&self, tool_use_id: &str) -> bool {
        self.tools.get(tool_use_id).is_some_and(|(name, _)| !is_agent_tool(name))
    }

    pub fn map(&mut self, raw: Raw) -> Vec<Event> {
        match raw {
            Raw::Init { session_id, model, permission_mode, cwd } => {
                if !cwd.is_empty() {
                    self.cwd = cwd.clone();
                }
                vec![Event::Started {
                    session_id: Some(session_id).filter(|s| !s.is_empty()),
                    model: Some(model).filter(|s| !s.is_empty()),
                    permission_mode: Some(permission_mode).filter(|s| !s.is_empty()),
                    cwd: Some(cwd).filter(|s| !s.is_empty()),
                }]
            }
            Raw::Status(s) => vec![Event::Status(s)],
            Raw::BlockStart { kind, tool_name, tool_id, .. } => match kind.as_str() {
                "text" => vec![Event::TextStart],
                "thinking" => vec![Event::Status("thinking".into())],
                "tool_use" => match (tool_id, tool_name) {
                    (Some(id), Some(name)) => {
                        self.tools.insert(id.clone(), (name.clone(), Value::Null));
                        vec![Event::ToolCall {
                            id,
                            name,
                            summary: String::new(),
                            input: Value::Object(Default::default()),
                            diff: None,
                            parent: None,
                        }]
                    }
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            },
            Raw::TextDelta { text, .. } => vec![Event::TextDelta(text)],
            Raw::Assistant { blocks, parent_tool_use_id: parent, model, context_tokens, error, .. } => {
                if parent.is_none()
                    && let Some(n) = context_tokens.filter(|n| *n > 0)
                {
                    self.context = Some(n);
                }
                let mut out = Vec::new();
                if parent.is_none() && !model.is_empty() && model != "<synthetic>" {
                    out.push(Event::Started { session_id: None, model: Some(model), permission_mode: None, cwd: None });
                }
                for block in blocks {
                    match block {
                        Block::Text(text) => out.push(Event::Text { text, parent: parent.clone() }),
                        Block::Thinking(_) => {}
                        Block::ToolUse { id, name, input } => {
                            self.tools.insert(id.clone(), (name.clone(), input.clone()));
                            let agent = is_agent_tool(&name) && parent.is_none();
                            let started = agent.then(|| {
                                let workflow = name == WORKFLOW;
                                Event::SubagentStarted {
                                    id: id.clone(),
                                    description: if workflow { workflow_title(&input) } else { s(&input, "description") },
                                    kind: if workflow { "workflow".into() } else { s(&input, "subagent_type") },
                                }
                            });
                            out.push(Event::ToolCall {
                                summary: tool_summary(&name, &input, &self.cwd),
                                diff: diff::from_input(&name, &input),
                                id,
                                name,
                                input,
                                parent: parent.clone(),
                            });
                            out.extend(started);
                        }
                    }
                }
                // After its words: the card offering the sign-in follows them.
                match error.as_deref() {
                    Some("authentication_failed") => out.push(Event::SignedOut),
                    // The limit refused it: when it resets, from the last limit event.
                    Some("rate_limit") => out.push(Event::Limit { reached: true, resets_at: self.limit_resets }),
                    _ => {}
                }
                out
            }
            Raw::ToolResult { tool_use_id, content, is_error, structured, parent_tool_use_id, images } => {
                let (name, input) = self.tools.get(&tool_use_id).cloned().unwrap_or_default();
                let diff = if is_error { None } else { diff::from_tool(&name, &input, structured.as_ref()) };
                let file_changed = diff.as_ref().map(|d| d.path.clone());
                // A subagent sent to the background: the call returns at once, the agent runs on
                // and reports later (see [`super::history`]).
                let background = is_agent_tool(&name)
                    && !is_error
                    && (content.starts_with(ASYNC_LAUNCHED) || content.starts_with(WORKFLOW_LAUNCHED));
                let mut out = Vec::new();
                if background {
                    let agent_id = launched_agent_id(&content).or_else(|| workflow_task_id(&content));
                    // Its end may name it only by this id.
                    if let Some(a) = &agent_id {
                        self.tasks.insert(a.clone(), tool_use_id.clone());
                    }
                    out.push(Event::SubagentBackground { id: tool_use_id.clone(), agent_id });
                }
                // A background subagent sent another message runs again.
                if name == "SendMessage"
                    && !is_error
                    && let Some(agent_id) = resumed_agent_id(&content)
                {
                    out.push(Event::SubagentResumed { agent_id });
                }
                // A known needless failure: say so, with the fixes.
                let issue = super::issues::spot(&name, &content, std::path::Path::new(&self.cwd), dirs::home_dir().as_deref());
                out.push(Event::ToolResult {
                    id: tool_use_id.clone(),
                    output: content,
                    is_error,
                    diff,
                    file_changed,
                    parent: parent_tool_use_id,
                });
                if let Some(issue) = issue {
                    out.push(Event::ToolIssue { id: tool_use_id.clone(), issue });
                }
                if !images.is_empty() {
                    out.push(Event::ToolImages { id: tool_use_id.clone(), images });
                }
                if is_agent_tool(&name) && !background {
                    out.push(Event::SubagentEnded {
                        id: tool_use_id,
                        failed: is_error,
                        summary: None,
                        tokens: None,
                        duration_ms: None,
                    });
                }
                out
            }
            Raw::UserText(text) if self.history => vec![Event::UserText(text)],
            Raw::UserText(_) => Vec::new(),
            Raw::PermissionRequest { request_id, tool_name, input, description, .. } => vec![Event::Permission {
                summary: tool_summary(&tool_name, &input, &self.cwd),
                request_id,
                tool: tool_name,
                description,
                input,
            }],
            Raw::TaskStarted { tool_use_id, .. } | Raw::TaskProgress { tool_use_id, .. } if self.other_tool(&tool_use_id) => {
                Vec::new()
            }
            Raw::TaskStarted { tool_use_id, task_id, description, subagent_type } => {
                self.tasks.insert(task_id, tool_use_id.clone());
                vec![Event::SubagentStarted { id: tool_use_id, description, kind: subagent_type }]
            }
            Raw::TaskProgress { tool_use_id, task_id, last_tool, total_tokens, tool_uses, duration_ms, .. } => {
                self.tasks.insert(task_id, tool_use_id.clone());
                vec![Event::SubagentProgress {
                    id: tool_use_id,
                    current_tool: last_tool,
                    tokens: total_tokens,
                    tool_uses,
                    duration_ms,
                }]
            }
            Raw::TaskDone { tool_use_id, task_id, status, summary, total_tokens, duration_ms } => {
                // Unknown here (launched by an earlier process): the id it was launched under,
                // which the chat knows it by.
                let id = tool_use_id.or_else(|| self.tasks.get(&task_id).cloned()).unwrap_or(task_id);
                if self.other_tool(&id) {
                    return Vec::new();
                }
                vec![Event::SubagentEnded {
                    id,
                    failed: matches!(status.as_str(), "failed" | "error" | "killed" | "cancelled" | "stopped"),
                    summary,
                    tokens: total_tokens,
                    duration_ms,
                }]
            }
            Raw::Result {
                is_error, subtype, text, total_cost_usd, duration_ms, input_tokens, output_tokens, session_id, context_window, ..
            } => {
                let window = context_window.map(Event::ContextWindow);
                window.into_iter().chain([Event::TurnEnded {
                    error: is_error.then(|| text.unwrap_or(subtype)),
                    cost_total_usd: total_cost_usd,
                    // The context's size at the last request, not the turn's total.
                    input_tokens: self.context.take().unwrap_or(input_tokens),
                    output_tokens,
                    duration_ms,
                    session_id: Some(session_id).filter(|s| !s.is_empty()),
                }])
                .collect()
            }
            Raw::Unparsed(line) => vec![Event::Notice(line)],
            Raw::RateLimit { status, resets_at } => {
                if resets_at.is_some() {
                    self.limit_resets = resets_at;
                }
                vec![Event::Limit { reached: status == "rejected", resets_at }]
            }
            Raw::Other(_) => Vec::new(),
        }
    }

    /// Every line of a recorded stream, mapped.
    pub fn map_lines(&mut self, text: &str) -> Vec<Event> {
        text.lines().filter_map(super::stream::parse_line).flat_map(|e| self.map(e)).collect()
    }
}

/// The agent a `SendMessage` result says it resumed (`{"resumedAgentId": ...}`).
fn resumed_agent_id(result: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(result.trim()).ok()?;
    v.get("resumedAgentId")?.as_str().map(str::to_string)
}

/// How the Agent tool's result begins when the subagent runs in the background.
const ASYNC_LAUNCHED: &str = "Async agent launched";

/// The `agentId: <id>` line of an Agent tool result: the id its report comes back under.
/// How the Workflow tool's result begins: the workflow runs on in the background.
const WORKFLOW_LAUNCHED: &str = "Workflow launched in background";

/// The task id a workflow was launched under (its end may name it only by that).
fn workflow_task_id(result: &str) -> Option<String> {
    result.lines().find_map(|l| Some(l.trim().strip_prefix("Task ID:")?.trim().to_string()))
}

/// A workflow's name for its card: its script's `description` (else `name`), from the script's
/// `meta`, else the name of a saved workflow it runs.
fn workflow_title(input: &Value) -> String {
    let script = s(input, "script");
    let field = |key: &str| {
        let at = script.find(&format!("{key}:"))? + key.len() + 1;
        let rest = script[at..].trim_start();
        let quote = rest.chars().next().filter(|c| matches!(c, '\'' | '"' | '`'))?;
        let end = rest[1..].find(quote)?;
        Some(rest[1..1 + end].to_string())
    };
    field("description")
        .or_else(|| field("name"))
        .or_else(|| Some(s(input, "name")).filter(|n| !n.is_empty()))
        .unwrap_or_else(|| "Workflow".into())
}

fn launched_agent_id(result: &str) -> Option<String> {
    result.lines().find_map(|l| {
        let id = l.trim().strip_prefix("agentId:")?.split_whitespace().next()?;
        Some(id.to_string())
    })
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

#[cfg(test)]
mod sign_in_tests {
    use super::*;
    use crate::conversation::{Conversation, Item, SignIn};

    /// What Claude Code 2.1 sends signed out (`claude -p` with an empty config folder).
    const SIGNED_OUT: &str = r#"{"type":"assistant","message":{"id":"x","model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"Not logged in · Please run /login"}]},"parent_tool_use_id":null,"session_id":"s","error":"authentication_failed","is_api_error_message":true}
{"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login","session_id":"s","terminal_reason":"api_error","duration_ms":43,"total_cost_usd":0,"usage":{"input_tokens":0,"output_tokens":0}}"#;

    #[test]
    fn signed_out_is_known_by_its_code_and_gets_one_card() {
        let events = Mapper::new("/p").map_lines(SIGNED_OUT);
        assert!(events.iter().any(|e| matches!(e, Event::SignedOut)), "{events:?}");
        assert!(!events.iter().any(|e| matches!(e, Event::Started { model: Some(m), .. } if m == "<synthetic>")), "not a model");
        // In another language the words change, the code does not.
        let other = SIGNED_OUT.replace("Not logged in · Please run /login", "Nicht angemeldet");
        assert!(Mapper::new("/p").map_lines(&other).iter().any(|e| matches!(e, Event::SignedOut)));
        let mut chat = Conversation::default();
        chat.apply_all(events.clone());
        chat.apply_all(events);
        let cards = chat.items.iter().filter(|i| matches!(i, Item::SignIn(SignIn::Needed))).count();
        assert_eq!(cards, 1, "a second refusal adds no second card");
    }
}

#[cfg(test)]
mod limit_tests {
    use super::*;
    use crate::conversation::Conversation;

    #[test]
    fn a_refusal_by_the_usage_limit_carries_when_it_resets() {
        let lines = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1793031600,"rateLimitType":"five_hour"}}
{"type":"assistant","message":{"id":"x","model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"Limite atteinte"}]},"parent_tool_use_id":null,"session_id":"s","error":"rate_limit","is_api_error_message":true}"#;
        let mut chat = Conversation::default();
        chat.apply_all(Mapper::new("/p").map_lines(lines));
        assert_eq!(chat.limit_reached, Some(Some(1793031600)), "from the codes, whatever the words");
        chat.apply_all(Mapper::new("/p").map_lines(r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1793050000}}"#));
        assert_eq!(chat.limit_reached, None, "allowed again");
    }
}

#[cfg(test)]
mod status_check {
    /// Runs the real agents (so ignored): `KL_EXPECT=true|false cargo test status_check -- --ignored`.
    #[test]
    #[ignore]
    fn both_agents_say_whether_they_are_signed_in() {
        use crate::agent::Backend;
        let want = std::env::var("KL_EXPECT").ok().map(|v| v == "true");
        let host = crate::host::LocalHost;
        let claude = crate::backends::claude::ClaudeBackend::default().signed_in(&host);
        let cursor = crate::backends::cursor::CursorBackend::default().signed_in(&host);
        eprintln!("claude {claude:?} cursor {cursor:?}");
        assert_eq!((claude, cursor), (want, want));
    }
}
