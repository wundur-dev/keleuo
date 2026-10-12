//! The chat as Keleion shows it, built by applying neutral agent events in order.

use std::path::PathBuf;

use serde_json::Value;

use crate::agent::Event;
use crate::diff::FileDiff;

/// The first line of `text`, cut to `max` characters with an ellipsis.
pub fn one_line(text: &str, max: usize) -> String {
    let first = text.lines().next().unwrap_or_default().trim();
    let more_lines = text.trim().lines().count() > 1;
    if first.chars().count() > max {
        let cut: String = first.chars().take(max).collect();
        format!("{cut}...")
    } else if more_lines {
        format!("{first} ...")
    } else {
        first.to_string()
    }
}

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
    /// Images the person sent with the message before.
    Images(Vec<PathBuf>),
    /// A stretch of earlier conversation not read yet (bytes `from..to` of the session file):
    /// read when it comes into view.
    Earlier { from: u64, to: u64, messages: usize },
    /// The context was compacted here: what came before was replaced by `summary`.
    Compacted { summary: String, trigger: Option<String>, tokens: Option<u64> },
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

/// What applying an event changed that the window cares about beyond the chat itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Applied {
    /// A file the agent wrote or edited (refresh it if open).
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
    /// The cost is computed from token counts and published prices, not reported by the agent.
    pub cost_estimated: bool,
    /// The total when the current turn began (for an estimated turn cost).
    turn_cost_from: f64,
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
        self.turn_cost_from = self.total_cost_usd;
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
        let (description, kind) = (String::new(), String::new());
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

    /// Apply one event.
    pub fn apply(&mut self, event: Event) -> Applied {
        let mut applied = Applied::default();
        self.revision += 1;
        match event {
            Event::Started { session_id, model, permission_mode, cwd } => {
                self.session_id = session_id.or(self.session_id.take());
                self.model = model.or(self.model.take());
                self.permission_mode = permission_mode.or(self.permission_mode.take());
                if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
                    self.cwd = cwd;
                }
            }
            Event::Status(status) => self.status = Some(status),
            Event::TextStart => {
                self.finish_streaming();
                self.items.push(Item::Assistant { text: String::new(), streaming: true });
                self.streaming = Some(self.items.len() - 1);
            }
            Event::TextDelta(text) => {
                if self.streaming.is_none() {
                    self.items.push(Item::Assistant { text: String::new(), streaming: true });
                    self.streaming = Some(self.items.len() - 1);
                }
                if let Some(Item::Assistant { text: t, .. }) = self.streaming.and_then(|i| self.items.get_mut(i)) {
                    t.push_str(&text);
                }
                self.status = Some("writing".into());
            }
            Event::Text { text, parent: Some(parent) } => {
                if !text.trim().is_empty() {
                    self.agent_entry(&parent).items.push(Item::Assistant { text, streaming: false });
                }
            }
            Event::Text { text, parent: None } => {
                if let Some(Item::Assistant { text: t, streaming }) =
                    self.streaming.take().and_then(|i| self.items.get_mut(i))
                {
                    *t = text;
                    *streaming = false;
                } else if !text.trim().is_empty() {
                    self.items.push(Item::Assistant { text, streaming: false });
                }
            }
            Event::UserText(text) => {
                self.finish_streaming();
                self.items.push(Item::User(text));
                self.turn_agents_from = self.agents.len();
                // Live (a session followed from its file): a turn has begun.
                if !self.replaying {
                    self.busy = true;
                    self.status = Some("working".into());
                    self.turn_cost_from = self.total_cost_usd;
                }
            }
            Event::ToolCall { id, name, summary, input, diff, parent } => {
                if parent.is_none() {
                    self.finish_streaming();
                    self.status = Some("running tool".into());
                }
                if let Some(tool) = self.tool_mut(&id) {
                    tool.summary = summary;
                    tool.input = input;
                    if diff.is_some() {
                        tool.diff = diff;
                    }
                    tool.name = name;
                    return applied;
                }
                let tool = Item::Tool(ToolCall {
                    id,
                    name: name.clone(),
                    input,
                    summary,
                    result: None,
                    is_error: false,
                    diff,
                    parent: parent.clone(),
                });
                match parent {
                    Some(p) => {
                        let agent = self.agent_entry(&p);
                        agent.current_tool = Some(name);
                        agent.items.push(tool);
                    }
                    None => self.items.push(tool),
                }
            }
            Event::ToolResult { id, output, is_error, diff, file_changed, parent } => {
                if let Some(parent) = &parent
                    && let Some(agent) = self.agents.iter_mut().find(|a| &a.tool_use_id == parent)
                {
                    agent.current_tool = None;
                }
                // The result of the call that started a subagent: it is done.
                if let Some(agent) = self.agents.iter_mut().find(|a| a.tool_use_id == id)
                    && agent.running()
                {
                    agent.status = if is_error { AgentStatus::Failed } else { AgentStatus::Done };
                    agent.current_tool = None;
                    if agent.duration_ms.is_none() {
                        agent.duration_ms = agent.started.map(|t| t.elapsed().as_millis() as u64);
                    }
                }
                if !is_error {
                    applied.file_changed = file_changed;
                }
                if let Some(tool) = self.tool_mut(&id) {
                    if is_error {
                        tool.diff = None;
                    } else if diff.is_some() {
                        tool.diff = diff;
                    }
                    tool.result = Some(output);
                    tool.is_error = is_error;
                }
            }
            Event::Permission { request_id, tool, summary, description, input } => {
                self.finish_streaming();
                self.items.push(Item::Permission(PermissionPrompt {
                    request_id,
                    tool_name: tool,
                    summary,
                    description,
                    input,
                    answer: None,
                }));
                self.status = Some("waiting for permission".into());
            }
            Event::SubagentStarted { id, description, kind } => {
                let agent = self.agent_entry(&id);
                if !description.is_empty() {
                    agent.description = description;
                }
                if !kind.is_empty() {
                    agent.kind = kind;
                }
            }
            Event::SubagentProgress { id, current_tool, tokens, tool_uses, duration_ms } => {
                let agent = self.agent_entry(&id);
                if current_tool.is_some() {
                    agent.current_tool = current_tool;
                }
                agent.total_tokens = tokens.or(agent.total_tokens);
                agent.tool_uses = tool_uses.or(agent.tool_uses);
                agent.duration_ms = duration_ms.or(agent.duration_ms);
            }
            Event::SubagentEnded { id, failed, summary, tokens, duration_ms } => {
                let agent = self.agent_entry(&id);
                agent.status = if failed { AgentStatus::Failed } else { AgentStatus::Done };
                agent.current_tool = None;
                agent.summary = summary.or(agent.summary.take());
                agent.total_tokens = tokens.or(agent.total_tokens);
                agent.duration_ms =
                    duration_ms.or(agent.duration_ms).or_else(|| agent.started.map(|t| t.elapsed().as_millis() as u64));
            }
            Event::TurnEnded { error, cost_total_usd, input_tokens, output_tokens, duration_ms, session_id } => {
                self.finish_streaming();
                self.busy = false;
                self.status = None;
                applied.turn_ended = true;
                if let Some(id) = session_id.filter(|s| !s.is_empty()) {
                    self.session_id = Some(id);
                }
                if let Some(total) = cost_total_usd {
                    let total = self.cost_base + total;
                    self.last_turn_cost_usd = Some((total - self.total_cost_usd).max(0.0));
                    self.total_cost_usd = total;
                } else if self.cost_estimated {
                    self.last_turn_cost_usd = Some((self.total_cost_usd - self.turn_cost_from).max(0.0));
                }
                self.last_turn_tokens = Some((input_tokens, output_tokens));
                self.last_turn_ms = duration_ms;
                if let Some(e) = error {
                    self.items.push(Item::Error(format!("The turn ended with an error: {}", one_line(&e, 300))));
                }
            }
            Event::UserImages(paths) => {
                if !paths.is_empty() {
                    self.items.push(Item::Images(paths));
                }
            }
            Event::CostEstimate { usd } => {
                self.total_cost_usd += usd;
                self.cost_estimated = true;
            }
            Event::Notice(line) => {
                if !line.trim().is_empty() {
                    self.items.push(Item::Notice(one_line(&line, 300)));
                }
            }
        }
        applied
    }

    /// Apply a list of events (replays and tests).
    /// A recorded history has been applied: no turn is running.
    pub fn finish_replay(&mut self) {
        self.finish_streaming();
        self.busy = false;
        self.status = None;
        self.replaying = false;
    }

    pub fn apply_all(&mut self, events: impl IntoIterator<Item = Event>) -> Vec<Applied> {
        events.into_iter().map(|e| self.apply(e)).collect()
    }

    /// The last thing that happened, as one line: the status while busy, else the agent's last
    /// words.
    pub fn last_activity(&self, max: usize) -> Option<String> {
        if self.needs_input() {
            return Some("waiting for permission".into());
        }
        if self.busy {
            let tool = self.items.iter().rev().find_map(|i| match i {
                Item::Tool(t) if !t.done() => Some(format!("{} {}", t.name, t.summary)),
                _ => None,
            });
            return Some(one_line(&tool.or_else(|| self.status.clone()).unwrap_or_else(|| "working".into()), max));
        }
        self.items.iter().rev().find_map(|i| match i {
            Item::Assistant { text, .. } if !text.trim().is_empty() => Some(one_line(text.trim(), max)),
            Item::Error(e) => Some(one_line(e, max)),
            _ => None,
        })
    }
}
