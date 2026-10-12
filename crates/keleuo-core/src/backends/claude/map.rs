//! Claude Code's `stream-json` events onto the neutral [`crate::agent::Event`]s. Claude's
//! tool names live here: Edit, MultiEdit and Write give diffs, Agent and Task start subagents.

use std::collections::HashMap;

use serde_json::Value;

use super::stream::{Block, Event as Raw};
use super::summary::tool_summary;
use crate::agent::Event;
use crate::diff;

/// Is this the tool that starts subagents?
pub fn is_agent_tool(name: &str) -> bool {
    matches!(name, "Agent" | "Task")
}

/// Keeps what the mapping needs between events: the session folder (for short paths), each
/// tool call's name and input (for the diff of its result), task ids to tool calls.
#[derive(Default)]
pub struct Mapper {
    cwd: String,
    tools: HashMap<String, (String, Value)>,
    tasks: HashMap<String, String>,
    /// Replaying a session file: the person's messages come through as text.
    pub history: bool,
}

impl Mapper {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self { cwd: cwd.into(), ..Default::default() }
    }

    /// Whether a tool call seen so far started a subagent.
    pub fn started_agent(&self, tool_use_id: &str) -> bool {
        self.tools.get(tool_use_id).is_some_and(|(name, _)| is_agent_tool(name))
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
            Raw::Assistant { blocks, parent_tool_use_id: parent, model, .. } => {
                let mut out = Vec::new();
                if parent.is_none() && !model.is_empty() {
                    out.push(Event::Started { session_id: None, model: Some(model), permission_mode: None, cwd: None });
                }
                for block in blocks {
                    match block {
                        Block::Text(text) => out.push(Event::Text { text, parent: parent.clone() }),
                        Block::Thinking(_) => {}
                        Block::ToolUse { id, name, input } => {
                            self.tools.insert(id.clone(), (name.clone(), input.clone()));
                            let agent = is_agent_tool(&name) && parent.is_none();
                            let started = agent.then(|| Event::SubagentStarted {
                                id: id.clone(),
                                description: s(&input, "description"),
                                kind: s(&input, "subagent_type"),
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
                out
            }
            Raw::ToolResult { tool_use_id, content, is_error, structured, parent_tool_use_id } => {
                let (name, input) = self.tools.get(&tool_use_id).cloned().unwrap_or_default();
                let diff = if is_error { None } else { diff::from_tool(&name, &input, structured.as_ref()) };
                let file_changed = diff.as_ref().map(|d| d.path.clone());
                // A subagent sent to the background: the call returns at once, the agent runs on
                // and reports later (see [`super::history`]).
                let background = is_agent_tool(&name) && !is_error && content.starts_with(ASYNC_LAUNCHED);
                let mut out = Vec::new();
                if background {
                    out.push(Event::SubagentBackground { id: tool_use_id.clone(), agent_id: launched_agent_id(&content) });
                }
                out.push(Event::ToolResult {
                    id: tool_use_id.clone(),
                    output: content,
                    is_error,
                    diff,
                    file_changed,
                    parent: parent_tool_use_id,
                });
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
                let Some(id) = tool_use_id.or_else(|| self.tasks.get(&task_id).cloned()) else {
                    return Vec::new();
                };
                vec![Event::SubagentEnded {
                    id,
                    failed: matches!(status.as_str(), "failed" | "error" | "killed" | "cancelled"),
                    summary,
                    tokens: total_tokens,
                    duration_ms,
                }]
            }
            Raw::Result { is_error, subtype, text, total_cost_usd, duration_ms, input_tokens, output_tokens, session_id, .. } => {
                vec![Event::TurnEnded {
                    error: is_error.then(|| text.unwrap_or(subtype)),
                    cost_total_usd: total_cost_usd,
                    input_tokens,
                    output_tokens,
                    duration_ms,
                    session_id: Some(session_id).filter(|s| !s.is_empty()),
                }]
            }
            Raw::Unparsed(line) => vec![Event::Notice(line)],
            Raw::Other(_) => Vec::new(),
        }
    }

    /// Every line of a recorded stream, mapped.
    pub fn map_lines(&mut self, text: &str) -> Vec<Event> {
        text.lines().filter_map(super::stream::parse_line).flat_map(|e| self.map(e)).collect()
    }
}

/// How the Agent tool's result begins when the subagent runs in the background.
const ASYNC_LAUNCHED: &str = "Async agent launched";

/// The `agentId: <id>` line of an Agent tool result: the id its report comes back under.
fn launched_agent_id(result: &str) -> Option<String> {
    result.lines().find_map(|l| {
        let id = l.trim().strip_prefix("agentId:")?.split_whitespace().next()?;
        Some(id.to_string())
    })
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}
