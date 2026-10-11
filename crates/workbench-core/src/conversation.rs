//! The chat as Workbench shows it, built by applying stream events in order.

use std::path::PathBuf;

use serde_json::Value;

use crate::diff::{self, FileDiff};
use crate::stream::{self, Block, Event};
use crate::summary::{one_line, tool_summary};

#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
    pub summary: String,
    /// The result text, once it arrived.
    pub result: Option<String>,
    pub is_error: bool,
    /// For Edit, MultiEdit and Write: what changed.
    pub diff: Option<FileDiff>,
    /// Set when the call was made by a subagent (the Task tool call it belongs to).
    pub parent: Option<String>,
}

impl ToolCall {
    pub fn done(&self) -> bool {
        self.result.is_some()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PermissionPrompt {
    pub request_id: String,
    pub tool_name: String,
    pub summary: String,
    pub description: Option<String>,
    pub input: Value,
    /// `Some(true)` allowed, `Some(false)` denied, `None` still waiting.
    pub answer: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    User(String),
    Assistant {
        text: String,
        streaming: bool,
    },
    Tool(ToolCall),
    Permission(PermissionPrompt),
    /// Turn summaries and other quiet notes.
    Notice(String),
    Error(String),
}

/// What applying an event changed that the window cares about beyond the chat itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Applied {
    /// A file Claude wrote or edited (refresh it if open).
    pub file_changed: Option<PathBuf>,
    /// A turn ended.
    pub turn_ended: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Conversation {
    pub items: Vec<Item>,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub cwd: String,
    /// A turn is in flight.
    pub busy: bool,
    /// Process total so far, as reported by the last result.
    pub total_cost_usd: f64,
    /// What the last finished turn cost.
    pub last_turn_cost_usd: Option<f64>,
    pub last_turn_tokens: Option<(u64, u64)>,
    pub last_turn_ms: Option<u64>,
    /// The latest status note (`requesting`, ...), cleared at the end of a turn.
    pub status: Option<String>,
    /// Index of the assistant text item receiving streamed deltas.
    streaming: Option<usize>,
    /// Cost total at the start of the process run (the result total restarts with a new process).
    cost_base: f64,
    /// Bumped on every change, so a view can tell when to redraw.
    pub revision: u64,
}

impl Conversation {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            cwd: cwd.into(),
            ..Default::default()
        }
    }

    /// Record a message the user sent.
    pub fn push_user(&mut self, text: &str) {
        self.items.push(Item::User(text.to_string()));
        self.busy = true;
        self.status = Some("sending".into());
        self.revision += 1;
    }

    pub fn push_error(&mut self, text: impl Into<String>) {
        self.items.push(Item::Error(text.into()));
        self.revision += 1;
    }

    pub fn push_notice(&mut self, text: impl Into<String>) {
        self.items.push(Item::Notice(text.into()));
        self.revision += 1;
    }

    /// The process ended: the next one reports its cost from zero again.
    pub fn process_ended(&mut self) {
        self.cost_base = self.total_cost_usd;
        self.busy = false;
        self.status = None;
        self.finish_streaming();
        self.revision += 1;
    }

    /// Answer a permission prompt; returns the line to write to Claude's stdin.
    pub fn answer_permission(&mut self, request_id: &str, allow: bool) -> Option<String> {
        let prompt = self.items.iter_mut().find_map(|i| match i {
            Item::Permission(p) if p.request_id == request_id && p.answer.is_none() => Some(p),
            _ => None,
        })?;
        prompt.answer = Some(allow);
        self.revision += 1;
        Some(stream::permission_response(
            request_id,
            allow,
            &prompt.input,
        ))
    }

    /// Permission prompts still waiting for an answer.
    pub fn pending_permissions(&self) -> impl Iterator<Item = &PermissionPrompt> {
        self.items.iter().filter_map(|i| match i {
            Item::Permission(p) if p.answer.is_none() => Some(p),
            _ => None,
        })
    }

    pub fn tool(&self, id: &str) -> Option<&ToolCall> {
        self.items.iter().find_map(|i| match i {
            Item::Tool(t) if t.id == id => Some(t),
            _ => None,
        })
    }

    fn tool_mut(&mut self, id: &str) -> Option<&mut ToolCall> {
        self.items.iter_mut().find_map(|i| match i {
            Item::Tool(t) if t.id == id => Some(t),
            _ => None,
        })
    }

    fn finish_streaming(&mut self) {
        if let Some(Item::Assistant { streaming, .. }) =
            self.streaming.take().and_then(|i| self.items.get_mut(i))
        {
            *streaming = false;
        }
    }

    fn new_tool(&mut self, id: String, name: String, input: Value, parent: Option<String>) {
        let summary = tool_summary(&name, &input, &self.cwd);
        self.items.push(Item::Tool(ToolCall {
            diff: diff::from_input(&name, &input),
            id,
            name,
            input,
            summary,
            result: None,
            is_error: false,
            parent,
        }));
    }

    /// Apply one event from the stream.
    pub fn apply(&mut self, event: Event) -> Applied {
        let mut applied = Applied::default();
        self.revision += 1;
        match event {
            Event::Init {
                session_id,
                model,
                permission_mode,
                cwd,
            } => {
                self.session_id = Some(session_id);
                self.model = Some(model);
                self.permission_mode = Some(permission_mode);
                if !cwd.is_empty() {
                    self.cwd = cwd;
                }
            }
            Event::Status(status) => self.status = Some(status),
            Event::BlockStart {
                kind,
                tool_name,
                tool_id,
                ..
            } => match kind.as_str() {
                "text" => {
                    self.finish_streaming();
                    self.items.push(Item::Assistant {
                        text: String::new(),
                        streaming: true,
                    });
                    self.streaming = Some(self.items.len() - 1);
                }
                "thinking" => self.status = Some("thinking".into()),
                "tool_use" => {
                    self.finish_streaming();
                    if let (Some(id), Some(name)) = (tool_id, tool_name)
                        && self.tool(&id).is_none()
                    {
                        self.new_tool(id, name, Value::Object(Default::default()), None);
                    }
                }
                _ => {}
            },
            Event::TextDelta { text, .. } => {
                if self.streaming.is_none() {
                    self.items.push(Item::Assistant {
                        text: String::new(),
                        streaming: true,
                    });
                    self.streaming = Some(self.items.len() - 1);
                }
                if let Some(Item::Assistant { text: t, .. }) =
                    self.streaming.and_then(|i| self.items.get_mut(i))
                {
                    t.push_str(&text);
                }
                self.status = Some("writing".into());
            }
            Event::Assistant {
                model,
                blocks,
                parent_tool_use_id,
                ..
            } => {
                if !model.is_empty() && parent_tool_use_id.is_none() {
                    self.model = Some(model);
                }
                for block in blocks {
                    match block {
                        Block::Text(text) => {
                            if let Some(Item::Assistant { text: t, streaming }) =
                                self.streaming.take().and_then(|i| self.items.get_mut(i))
                            {
                                *t = text;
                                *streaming = false;
                            } else if !text.trim().is_empty() {
                                self.items.push(Item::Assistant {
                                    text,
                                    streaming: false,
                                });
                            }
                        }
                        Block::Thinking(_) => {}
                        Block::ToolUse { id, name, input } => {
                            self.finish_streaming();
                            let cwd = self.cwd.clone();
                            if let Some(tool) = self.tool_mut(&id) {
                                tool.summary = tool_summary(&name, &input, &cwd);
                                tool.diff = diff::from_input(&name, &input);
                                tool.input = input;
                                tool.parent = parent_tool_use_id.clone();
                            } else {
                                self.new_tool(id, name, input, parent_tool_use_id.clone());
                            }
                            self.status = Some("running tool".into());
                        }
                    }
                }
            }
            Event::ToolResult {
                tool_use_id,
                content,
                is_error,
                structured,
            } => {
                if let Some(tool) = self.tool_mut(&tool_use_id) {
                    if !is_error
                        && let Some(d) =
                            diff::from_tool(&tool.name, &tool.input, structured.as_ref())
                    {
                        applied.file_changed = Some(d.path.clone());
                        tool.diff = Some(d);
                    }
                    if is_error {
                        tool.diff = None;
                    }
                    tool.result = Some(content);
                    tool.is_error = is_error;
                }
            }
            Event::UserText(_) => {}
            Event::PermissionRequest {
                request_id,
                tool_name,
                input,
                description,
                ..
            } => {
                self.finish_streaming();
                let summary = tool_summary(&tool_name, &input, &self.cwd);
                self.items.push(Item::Permission(PermissionPrompt {
                    request_id,
                    tool_name,
                    summary,
                    description,
                    input,
                    answer: None,
                }));
                self.status = Some("waiting for permission".into());
            }
            Event::Result {
                is_error,
                subtype,
                text,
                total_cost_usd,
                duration_ms,
                input_tokens,
                output_tokens,
                session_id,
                ..
            } => {
                self.finish_streaming();
                self.busy = false;
                self.status = None;
                applied.turn_ended = true;
                if !session_id.is_empty() {
                    self.session_id = Some(session_id);
                }
                if let Some(total) = total_cost_usd {
                    let total = self.cost_base + total;
                    self.last_turn_cost_usd = Some((total - self.total_cost_usd).max(0.0));
                    self.total_cost_usd = total;
                }
                self.last_turn_tokens = Some((input_tokens, output_tokens));
                self.last_turn_ms = duration_ms;
                if is_error {
                    let detail = text.unwrap_or(subtype);
                    self.items.push(Item::Error(format!(
                        "The turn ended with an error: {}",
                        one_line(&detail, 300)
                    )));
                }
            }
            Event::Unparsed(line) => {
                if !line.trim().is_empty() {
                    self.items.push(Item::Notice(one_line(&line, 300)));
                }
            }
            Event::Other(_) => {}
        }
        applied
    }

    /// Apply every line of a recorded stream (tests and replays).
    pub fn apply_lines(&mut self, text: &str) -> Vec<Applied> {
        text.lines()
            .filter_map(stream::parse_line)
            .map(|e| self.apply(e))
            .collect()
    }
}
