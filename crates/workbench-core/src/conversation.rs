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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentStatus {
    Running,
    Done,
    Failed,
}

/// A subagent: an Agent or Task tool call running its own conversation.
#[derive(Clone, Debug, PartialEq)]
pub struct SubAgent {
    /// The Agent or Task tool call that started it.
    pub tool_use_id: String,
    pub task_id: Option<String>,
    pub description: String,
    pub kind: String,
    pub status: AgentStatus,
    /// When it started (for the live elapsed time); `None` in replays.
    pub started: Option<std::time::Instant>,
    /// Reported duration so far, in milliseconds.
    pub duration_ms: Option<u64>,
    /// The tool it is using right now.
    pub current_tool: Option<String>,
    pub total_tokens: Option<u64>,
    pub tool_uses: Option<u64>,
    pub summary: Option<String>,
    /// Its own messages and tool calls.
    pub items: Vec<Item>,
}

impl SubAgent {
    pub fn running(&self) -> bool {
        self.status == AgentStatus::Running
    }

    /// Elapsed time: live while running, else what the stream reported.
    pub fn elapsed(&self) -> Option<std::time::Duration> {
        match (self.status, self.started) {
            (AgentStatus::Running, Some(t)) => Some(t.elapsed()),
            _ => self.duration_ms.map(std::time::Duration::from_millis),
        }
    }
}

/// Is this tool the one that starts subagents?
pub fn is_agent_tool(name: &str) -> bool {
    matches!(name, "Agent" | "Task")
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
    /// Subagents, in start order.
    pub agents: Vec<SubAgent>,
    /// Index into `agents` where the current (or last) turn's subagents begin.
    pub turn_agents_from: usize,
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
    /// Applying a recorded history: no live clocks.
    pub replaying: bool,
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
        self.turn_agents_from = self.agents.len();
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

    /// Waiting for the user: a permission prompt is open.
    pub fn needs_input(&self) -> bool {
        self.pending_permissions().next().is_some()
    }

    /// A short title: the first user message, cut to `max` characters.
    pub fn title(&self, max: usize) -> Option<String> {
        self.items.iter().find_map(|i| match i {
            Item::User(t) => Some(one_line(t, max)),
            _ => None,
        })
    }

    /// The process ended: the next one reports its cost from zero again.
    pub fn process_ended(&mut self) {
        self.cost_base = self.total_cost_usd;
        self.busy = false;
        self.status = None;
        self.finish_streaming();
        self.revision += 1;
    }

    /// Mark a permission prompt answered; returns the tool input it asked about (the agent
    /// sends the answer). `None` when there is no such open prompt.
    pub fn answer_permission(&mut self, request_id: &str, allow: bool) -> Option<Value> {
        let prompt = self.items.iter_mut().find_map(|i| match i {
            Item::Permission(p) if p.request_id == request_id && p.answer.is_none() => Some(p),
            _ => None,
        })?;
        prompt.answer = Some(allow);
        self.revision += 1;
        Some(prompt.input.clone())
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
        if let Some(ix) = self
            .items
            .iter()
            .position(|i| matches!(i, Item::Tool(t) if t.id == id))
        {
            return match &mut self.items[ix] {
                Item::Tool(t) => Some(t),
                _ => None,
            };
        }
        for agent in &mut self.agents {
            if let Some(ix) = agent
                .items
                .iter()
                .position(|i| matches!(i, Item::Tool(t) if t.id == id))
            {
                return match &mut agent.items[ix] {
                    Item::Tool(t) => Some(t),
                    _ => None,
                };
            }
        }
        None
    }

    pub fn agent(&self, tool_use_id: &str) -> Option<&SubAgent> {
        self.agents.iter().find(|a| a.tool_use_id == tool_use_id)
    }

    /// The subagent for a tool call id, created when first seen.
    fn agent_entry(&mut self, tool_use_id: &str) -> &mut SubAgent {
        if let Some(ix) = self
            .agents
            .iter()
            .position(|a| a.tool_use_id == tool_use_id)
        {
            return &mut self.agents[ix];
        }
        let (description, kind) = match self.tool(tool_use_id) {
            Some(t) => (
                t.input
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                t.input
                    .get("subagent_type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            ),
            None => (String::new(), String::new()),
        };
        self.agents.push(SubAgent {
            tool_use_id: tool_use_id.to_string(),
            task_id: None,
            description,
            kind,
            status: AgentStatus::Running,
            started: (!self.replaying).then(std::time::Instant::now),
            duration_ms: None,
            current_tool: None,
            total_tokens: None,
            tool_uses: None,
            summary: None,
            items: Vec::new(),
        });
        self.agents.last_mut().unwrap()
    }

    fn agent_by_task(&mut self, task_id: &str) -> Option<&mut SubAgent> {
        self.agents
            .iter_mut()
            .find(|a| a.task_id.as_deref() == Some(task_id))
    }

    /// Subagents still running.
    pub fn running_agents(&self) -> impl Iterator<Item = &SubAgent> {
        self.agents.iter().filter(|a| a.running())
    }

    /// Subagents of the current (or last) turn.
    pub fn turn_agents(&self) -> &[SubAgent] {
        &self.agents[self.turn_agents_from.min(self.agents.len())..]
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
                if let Some(parent) = parent_tool_use_id {
                    let cwd = self.cwd.clone();
                    let agent = self.agent_entry(&parent);
                    for block in blocks {
                        match block {
                            Block::Text(text) if !text.trim().is_empty() => {
                                agent.items.push(Item::Assistant {
                                    text,
                                    streaming: false,
                                })
                            }
                            Block::ToolUse { id, name, input } => {
                                agent.current_tool = Some(name.clone());
                                agent.items.push(Item::Tool(ToolCall {
                                    summary: tool_summary(&name, &input, &cwd),
                                    diff: diff::from_input(&name, &input),
                                    id,
                                    name,
                                    input,
                                    result: None,
                                    is_error: false,
                                    parent: Some(parent.clone()),
                                }));
                            }
                            _ => {}
                        }
                    }
                    return applied;
                }
                if !model.is_empty() {
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
                                let agent_tool = is_agent_tool(&name);
                                self.new_tool(id.clone(), name, input, None);
                                if agent_tool {
                                    self.agent_entry(&id);
                                }
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
                parent_tool_use_id,
            } => {
                if let Some(parent) = &parent_tool_use_id
                    && let Some(agent) = self.agents.iter_mut().find(|a| &a.tool_use_id == parent)
                {
                    agent.current_tool = None;
                }
                // The Agent tool's own result: the subagent is done.
                if let Some(agent) = self
                    .agents
                    .iter_mut()
                    .find(|a| a.tool_use_id == tool_use_id)
                    && agent.running()
                {
                    agent.status = if is_error {
                        AgentStatus::Failed
                    } else {
                        AgentStatus::Done
                    };
                    agent.current_tool = None;
                    if agent.duration_ms.is_none() {
                        agent.duration_ms = agent.started.map(|t| t.elapsed().as_millis() as u64);
                    }
                }
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
            Event::TaskStarted {
                tool_use_id,
                task_id,
                description,
                subagent_type,
            } => {
                let agent = self.agent_entry(&tool_use_id);
                agent.task_id = Some(task_id);
                if !description.is_empty() {
                    agent.description = description;
                }
                if !subagent_type.is_empty() {
                    agent.kind = subagent_type;
                }
            }
            Event::TaskProgress {
                tool_use_id,
                task_id,
                last_tool,
                total_tokens,
                tool_uses,
                duration_ms,
                ..
            } => {
                let agent = self.agent_entry(&tool_use_id);
                agent.task_id.get_or_insert(task_id);
                if last_tool.is_some() {
                    agent.current_tool = last_tool;
                }
                agent.total_tokens = total_tokens.or(agent.total_tokens);
                agent.tool_uses = tool_uses.or(agent.tool_uses);
                agent.duration_ms = duration_ms.or(agent.duration_ms);
            }
            Event::TaskDone {
                tool_use_id,
                task_id,
                status,
                summary,
                total_tokens,
                duration_ms,
            } => {
                let agent = match tool_use_id {
                    Some(id) => Some(self.agent_entry(&id)),
                    None => self.agent_by_task(&task_id),
                };
                if let Some(agent) = agent {
                    agent.status = match status.as_str() {
                        "failed" | "error" | "killed" | "cancelled" => AgentStatus::Failed,
                        _ => AgentStatus::Done,
                    };
                    agent.current_tool = None;
                    agent.summary = summary.or(agent.summary.take());
                    agent.total_tokens = total_tokens.or(agent.total_tokens);
                    agent.duration_ms = duration_ms
                        .or(agent.duration_ms)
                        .or_else(|| agent.started.map(|t| t.elapsed().as_millis() as u64));
                }
            }
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
