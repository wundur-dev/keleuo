//! The chat as Keleuo shows it, built by applying neutral agent events in order.

use std::path::{Path, PathBuf};

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
    /// When it started (for the live elapsed time); `None` in replays.
    pub started: Option<std::time::Instant>,
    /// How long it took, once it ended (or its turn did).
    pub duration_ms: Option<u64>,
    /// What it has printed so far while it runs (the latest part); dropped when it ends.
    pub live: Option<String>,
    /// It failed in a known way, with fixes to offer.
    pub issue: Option<Box<crate::agent::Issue>>,
    /// Images it returned (see [`Event::ToolImages`]).
    pub images: Vec<PathBuf>,
}

impl ToolCall {
    pub fn done(&self) -> bool {
        self.result.is_some()
    }

    /// Running now, with a clock: started live, and neither finished nor stopped with its turn.
    pub fn running(&self) -> bool {
        !self.done() && self.duration_ms.is_none() && self.started.is_some()
    }

    /// Elapsed time: live while running, else how long it took (when known).
    pub fn elapsed(&self) -> Option<std::time::Duration> {
        match self.duration_ms {
            Some(ms) => Some(std::time::Duration::from_millis(ms)),
            None if !self.done() => self.started.map(|t| t.elapsed()),
            None => None,
        }
    }

    /// Stop its clock: it ended (or will not end any more).
    fn stop_clock(&mut self) {
        if self.duration_ms.is_none() {
            self.duration_ms = self.started.map(|t| t.elapsed().as_millis() as u64);
        }
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
    /// A question's answer, as the person gave it (see [`crate::questions`]).
    pub reply: Option<String>,
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
    /// A message from another agent: `from` its id, `agent` the subagent's tool call when it
    /// is one of this session's, `title` what it was asked to do (or its id).
    AgentMessage { from: String, agent: Option<String>, title: String, text: String },
    /// The files the turn just ended changed: the end-of-turn card.
    TurnChanges(Vec<crate::changes::FileChange>),
    /// A turn's summary (see [`crate::summary`]), at the end of its turn, above its card of
    /// changes. `key`: the turn's (see [`crate::summary::TurnText::key`]).
    Summary { key: String, state: crate::summary::State },
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
    /// It runs in the background: its tool call returned at once, and it ends later.
    pub background: bool,
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
    /// What to call it on one line: the first line of its description, else its type.
    pub fn title(&self) -> String {
        match self.description.lines().map(str::trim).find(|l| !l.is_empty()) {
            Some(line) => line.to_string(),
            None => self.kind.clone(),
        }
    }

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
    /// The model's context window, when the agent reported it.
    pub context_window: Option<u64>,
    /// The latest status note (`requesting`, ...), cleared at the end of a turn.
    pub status: Option<String>,
    /// Index of the assistant text item receiving streamed deltas.
    streaming: Option<usize>,
    /// The streaming item is a preview read off the agent's screen, not text from its stream:
    /// replaced by the finished text, removed if anything else comes first.
    preview: bool,
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
            Item::User(t) => Some(one_line(&crate::paste::title_text(t), max)),
            _ => None,
        })
    }

    /// The process ended: the next one reports its cost from zero again.
    pub fn process_ended(&mut self) {
        // Subagents run inside the agent's process: they end with it.
        for agent in self.agents.iter_mut().filter(|a| a.running()) {
            agent.status = AgentStatus::Failed;
            agent.current_tool = None;
            agent.summary = Some("Stopped: the agent's process ended (a restart or exit) before it finished.".into());
            if agent.duration_ms.is_none() {
                agent.duration_ms = agent.started.map(|t| t.elapsed().as_millis() as u64);
            }
        }
        self.cost_base = self.total_cost_usd;
        self.stop_clocks();
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

    /// Mark a question answered with `reply` (as shown); returns the tool input it asked with.
    pub fn answer_question(&mut self, request_id: &str, reply: String) -> Option<Value> {
        let input = self.answer_permission(request_id, true)?;
        if let Some(Item::Permission(p)) = self.items.iter_mut().find(|i| matches!(i, Item::Permission(p) if p.request_id == request_id)) {
            p.reply = Some(reply);
        }
        Some(input)
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
            background: false,
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

    /// Tool calls still running with a clock, the subagents' included.
    pub fn running_tools(&self) -> impl Iterator<Item = &ToolCall> {
        self.items
            .iter()
            .chain(self.agents.iter().flat_map(|a| a.items.iter()))
            .filter_map(|i| match i {
                Item::Tool(t) if t.running() => Some(t),
                _ => None,
            })
    }

    /// The turn ended: its unfinished tool calls stop their clocks (they were interrupted), and
    /// so do those of subagents no longer running. Background subagents keep theirs.
    fn stop_clocks(&mut self) {
        let finished_agents = self.agents.iter_mut().filter(|a| !a.running());
        for item in self.items.iter_mut().chain(finished_agents.flat_map(|a| a.items.iter_mut())) {
            if let Item::Tool(t) = item
                && !t.done()
            {
                t.stop_clock();
            }
        }
    }

    /// Subagents still running.
    pub fn running_agents(&self) -> impl Iterator<Item = &SubAgent> {
        self.agents.iter().filter(|a| a.running())
    }

    /// Subagents of the current (or last) turn.
    pub fn turn_agents(&self) -> &[SubAgent] {
        &self.agents[self.turn_agents_from.min(self.agents.len())..]
    }

    /// Every change this session made to `path` that went through, oldest first. A subagent's
    /// changes count from where its tool call is in the chat.
    pub fn file_changes(&self, path: &Path) -> Vec<FileDiff> {
        let mut out = Vec::new();
        self.collect_changes(&self.items, path, &mut out);
        out
    }

    /// Where the current (or last) turn starts: the person's last message.
    pub fn turn_start(&self) -> usize {
        self.items.iter().rposition(|i| matches!(i, Item::User(_))).unwrap_or(0)
    }

    /// The files changed by the edits in `self.items[from..]` (subagents' included), for the
    /// Changes panel: a turn from [`Self::turn_start`], the session from 0. Changes made by
    /// commands (a formatter, `sed`) are not tool edits and are not here.
    pub fn changes(&self, from: usize) -> Vec<crate::changes::FileChange> {
        let items = self.items.get(from..).unwrap_or(&[]);
        let mut diffs = Vec::new();
        self.collect_all_changes(items, &mut diffs);
        let mut out = crate::changes::summarize(diffs);
        // And what commands changed, as the turns' cards recorded it.
        let by_command = items.iter().filter_map(|i| if let Item::TurnChanges(c) = i { Some(c) } else { None }).flatten();
        for c in by_command.filter(|c| c.by_command) {
            match out.iter_mut().find(|o| o.path == c.path) {
                Some(o) if o.by_command => {
                    o.added += c.added;
                    o.removed += c.removed;
                }
                Some(_) => {}
                None => out.push(c.clone()),
            }
        }
        out.sort_by_key(|c| c.group);
        out
    }

    /// Add what commands changed in the turn that starts at `turn` (its first item; see
    /// [`Self::turn_start`]) to its end-of-turn card, making the card when the edit tools
    /// changed nothing. Found after the turn ended (git takes a moment); dropped if another
    /// turn has started since.
    pub fn add_command_changes(&mut self, turn: usize, changes: Vec<crate::changes::FileChange>) {
        if changes.is_empty() || self.turn_start() != turn {
            return;
        }
        let card = self.items[turn..].iter_mut().find_map(|i| if let Item::TurnChanges(c) = i { Some(c) } else { None });
        match card {
            Some(card) => {
                for c in changes {
                    if !card.iter().any(|k| k.path == c.path) {
                        card.push(c);
                    }
                }
                card.sort_by_key(|c| c.group);
            }
            None => self.items.push(Item::TurnChanges(changes)),
        }
        self.revision += 1;
    }

    /// Where the turn starting at `from` ends (the next message, or the end).
    pub fn turn_end(&self, from: usize) -> usize {
        self.items.iter().skip(from + 1).position(|i| matches!(i, Item::User(_))).map_or(self.items.len(), |p| from + 1 + p)
    }

    /// Show a turn's summary: the turn whose key is `key` (found again by it: the chat may
    /// have moved on, or earlier history been read in, since it was asked for). Nothing when
    /// the turn is gone.
    pub fn set_summary(&mut self, key: &str, state: crate::summary::State) {
        if let Some(Item::Summary { state: s, .. }) =
            self.items.iter_mut().find(|i| matches!(i, Item::Summary { key: k, .. } if k == key))
        {
            *s = state;
            self.revision += 1;
            return;
        }
        let starts: Vec<usize> = self.items.iter().enumerate().filter(|(_, i)| matches!(i, Item::User(_))).map(|(ix, _)| ix).collect();
        let Some(from) = starts.into_iter().rev().find(|&from| crate::summary::TurnText::key_at(self, from).as_deref() == Some(key))
        else {
            return;
        };
        let end = self.turn_end(from);
        let at = self.items[from..end].iter().position(|i| matches!(i, Item::TurnChanges(_))).map_or(end, |p| from + p);
        self.items.insert(at, Item::Summary { key: key.to_string(), state });
        self.revision += 1;
    }

    /// Show the kept summaries of the turns they belong to (a session read back from its file).
    pub fn attach_summaries(&mut self, kept: &std::collections::BTreeMap<String, crate::summary::TurnSummary>) {
        if kept.is_empty() {
            return;
        }
        let starts: Vec<usize> = self.items.iter().enumerate().filter(|(_, i)| matches!(i, Item::User(_))).map(|(ix, _)| ix).collect();
        for from in starts.into_iter().rev() {
            let end = self.turn_end(from);
            if self.items[from..end].iter().any(|i| matches!(i, Item::Summary { .. })) {
                continue;
            }
            if let Some(key) = crate::summary::TurnText::key_at(self, from)
                && let Some(s) = kept.get(&key)
            {
                let at = self.items[from..end].iter().position(|i| matches!(i, Item::TurnChanges(_))).map_or(end, |p| from + p);
                self.items.insert(at, Item::Summary { key, state: crate::summary::State::Ready(s.clone()) });
                self.revision += 1;
            }
        }
    }

    /// The latest turn's summary, once it is there.
    pub fn last_summary(&self) -> Option<&crate::summary::TurnSummary> {
        self.items[self.turn_start()..].iter().find_map(|i| match i {
            Item::Summary { state: crate::summary::State::Ready(s), .. } => Some(s),
            _ => None,
        })
    }

    /// The files the agent's edit tools changed in the turn starting at `turn`.
    pub fn edited_in_turn(&self, turn: usize) -> std::collections::HashSet<PathBuf> {
        self.changes(turn).into_iter().filter(|c| !c.by_command).map(|c| c.path).collect()
    }

    /// End the turn with a card of the files it changed, if it changed any (once: the turn end
    /// can arrive twice, from a hook and from the session file).
    fn close_turn(&mut self) {
        let from = self.turn_start();
        if self.items[from..].iter().any(|i| matches!(i, Item::TurnChanges(_))) {
            return;
        }
        let changes = self.changes(from);
        if !changes.is_empty() {
            self.items.push(Item::TurnChanges(changes));
        }
    }

    fn collect_all_changes(&self, items: &[Item], out: &mut Vec<FileDiff>) {
        for item in items {
            let Item::Tool(t) = item else { continue };
            if let Some(agent) = self.agent(&t.id) {
                self.collect_all_changes(&agent.items, out);
            }
            if let Some(d) = &t.diff
                && t.done()
                && !t.is_error
            {
                out.push(d.clone());
            }
        }
    }

    /// Whether this session changed `path`.
    pub fn edited(&self, path: &Path) -> bool {
        !self.file_changes(path).is_empty()
    }

    fn collect_changes(&self, items: &[Item], path: &Path, out: &mut Vec<FileDiff>) {
        for item in items {
            // What commands changed, found when the turn ended.
            if let Item::TurnChanges(changes) = item {
                out.extend(changes.iter().filter(|c| c.path == path).filter_map(|c| c.diff.clone()));
                continue;
            }
            let Item::Tool(t) = item else { continue };
            if let Some(agent) = self.agent(&t.id) {
                self.collect_changes(&agent.items, path, out);
            }
            if let Some(d) = &t.diff
                && t.done()
                && !t.is_error
                && d.path == path
            {
                out.push(d.clone());
            }
        }
    }

    fn finish_streaming(&mut self) {
        if std::mem::take(&mut self.preview) {
            // A preview the finished text never replaced: it goes.
            if let Some(ix) = self.streaming.take().filter(|ix| *ix < self.items.len()) {
                self.items.remove(ix);
            }
            return;
        }
        if let Some(Item::Assistant { streaming, .. }) =
            self.streaming.take().and_then(|i| self.items.get_mut(i))
        {
            *streaming = false;
        }
    }

    /// The item shown as a preview of the reply being written, if any.
    pub fn preview_item(&self) -> Option<usize> {
        self.preview.then_some(self.streaming).flatten()
    }

    /// Whether the reply `text` (as a preview has it) is in this turn's replies already: the
    /// screen still shows a reply after its record has it.
    fn delivered(&self, text: &str) -> bool {
        let key: String = letters(text).chars().take(60).collect();
        if key.is_empty() {
            return true;
        }
        let from = self.items.iter().rposition(|i| matches!(i, Item::User(_))).map_or(0, |i| i + 1);
        self.items[from.min(self.items.len())..].iter().enumerate().any(|(k, item)| {
            Some(from + k) != self.preview_item() && matches!(item, Item::Assistant { text: t, .. } if letters(t).contains(&key))
        })
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
            Event::TextPreview(text) => {
                if self.delivered(&text) {
                    if self.preview {
                        self.finish_streaming();
                    }
                } else if let Some(Item::Assistant { text: t, .. }) = self.preview_item().and_then(|i| self.items.get_mut(i)) {
                    *t = text;
                } else if self.streaming.is_none() {
                    self.items.push(Item::Assistant { text, streaming: true });
                    self.streaming = Some(self.items.len() - 1);
                    self.preview = true;
                }
            }
            Event::Text { text, parent: None } => {
                // The finished text: it takes the place of the preview, if there is one.
                self.preview = false;
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
                    started: (!self.replaying).then(std::time::Instant::now),
                    duration_ms: None,
                    live: None,
                    issue: None,
                    images: Vec::new(),
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
                // The result of the call that started a subagent: it is done (unless it runs in
                // the background, when it only started).
                if let Some(agent) = self.agents.iter_mut().find(|a| a.tool_use_id == id)
                    && agent.running()
                    && !(agent.background && !is_error)
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
                    tool.live = None;
                    tool.stop_clock();
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
                    reply: None,
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
            Event::ToolIssue { id, issue } => {
                if let Some(tool) = self.tool_mut(&id) {
                    tool.issue = Some(Box::new(issue));
                }
            }
            Event::ToolImages { id, images } => {
                if let Some(tool) = self.tool_mut(&id) {
                    tool.images = images;
                }
            }
            Event::ContextWindow(n) => self.context_window = Some(n),
            Event::ToolOutput { id, text } => {
                if let Some(tool) = self.tool_mut(&id)
                    && !tool.done()
                {
                    tool.live = Some(text);
                }
            }
            Event::Interrupted => {
                self.finish_streaming();
                self.busy = false;
                self.status = None;
                self.items.push(Item::Notice("Interrupted".into()));
                applied.turn_ended = true;
            }
            Event::SubagentBackground { id, agent_id } => {
                let agent = self.agent_entry(&id);
                agent.background = true;
                agent.task_id = agent_id.or(agent.task_id.take());
            }
            Event::AgentMessage { from, text } => {
                self.finish_streaming();
                let agent = self.agents.iter_mut().find(|a| a.task_id.as_deref() == Some(from.as_str()));
                let (tool_use_id, title) = match agent {
                    Some(a) => {
                        // Its report is its last word.
                        if a.running() {
                            a.status = AgentStatus::Done;
                            a.current_tool = None;
                            if a.duration_ms.is_none() {
                                a.duration_ms = a.started.map(|t| t.elapsed().as_millis() as u64);
                            }
                        }
                        let title = if a.description.is_empty() { a.kind.clone() } else { a.description.clone() };
                        (Some(a.tool_use_id.clone()), title)
                    }
                    None => (None, from.clone()),
                };
                self.items.push(Item::AgentMessage { from, agent: tool_use_id, title, text });
            }
            Event::SubagentResumed { agent_id } => {
                let replaying = self.replaying;
                if let Some(agent) = self.agents.iter_mut().find(|a| a.task_id.as_deref() == Some(agent_id.as_str())) {
                    agent.status = AgentStatus::Running;
                    agent.summary = None;
                    agent.duration_ms = None;
                    agent.started = (!replaying).then(std::time::Instant::now);
                }
            }
            Event::SubagentEnded { id, failed, summary, tokens, duration_ms } => {
                // Named by the tool call that started it, or by the id it was launched under (a
                // background agent stopped from outside); the end of anything else (a background
                // command) is no subagent's.
                let Some(id) = self
                    .agents
                    .iter()
                    .find(|a| a.tool_use_id == id)
                    .or_else(|| self.agents.iter().find(|a| a.task_id.as_deref() == Some(id.as_str())))
                    .map(|a| a.tool_use_id.clone())
                else {
                    return applied;
                };
                let agent = self.agent_entry(&id);
                agent.status = if failed { AgentStatus::Failed } else { AgentStatus::Done };
                agent.current_tool = None;
                // Its unfinished calls end with it: their clocks stop (a background agent can
                // end after the turn did, and nothing else would stop them).
                for item in agent.items.iter_mut() {
                    if let Item::Tool(t) = item
                        && !t.done()
                    {
                        t.stop_clock();
                    }
                }
                agent.summary = summary.or(agent.summary.take());
                agent.total_tokens = tokens.or(agent.total_tokens);
                agent.duration_ms =
                    duration_ms.or(agent.duration_ms).or_else(|| agent.started.map(|t| t.elapsed().as_millis() as u64));
            }
            Event::TurnEnded { error, cost_total_usd, input_tokens, output_tokens, duration_ms, session_id } => {
                self.finish_streaming();
                self.stop_clocks();
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
                self.close_turn();
                if let Some(e) = error {
                    self.items.push(Item::Error(format!("The turn ended with an error: {}", one_line(&e, 300))));
                }
            }
            Event::UserImages(paths) => {
                // Above the message's text, as they were in the message box and as the agent
                // gets them (images first); the text is added first, so they go before it.
                if !paths.is_empty() {
                    match self.items.last() {
                        Some(Item::User(_)) => {
                            let at = self.items.len() - 1;
                            self.items.insert(at, Item::Images(paths));
                        }
                        _ => self.items.push(Item::Images(paths)),
                    }
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

/// The conversation as Markdown, for the clipboard: messages in full, tool calls one line each
/// (with a short excerpt of the result), reports, compactions and notes. `agent` names the
/// agent's replies.
pub fn to_markdown(items: &[Item], agent: &str) -> String {
    let mut out = String::new();
    let mut last_tool = false;
    for item in items {
        let block = match item {
            Item::User(text) => format!("### You\n\n{}", crate::paste::without_markers(text).trim()),
            Item::Assistant { text, .. } if text.trim().is_empty() => continue,
            Item::Assistant { text, .. } => format!("### {agent}\n\n{}", text.trim()),
            Item::Tool(t) => tool_line(t),
            Item::Permission(p) => {
                let answer = match p.answer {
                    Some(true) => "allowed",
                    Some(false) => "denied",
                    None => "waiting",
                };
                format!("- Permission for **{}** {}: {answer}", p.tool_name, one_line(&p.summary, 120))
            }
            Item::Notice(text) => format!("*{}*", text.trim()),
            // Keleuo's own note, not part of the conversation.
            Item::Summary { .. } => continue,
            Item::TurnChanges(changes) => {
                let names: Vec<String> = changes.iter().map(|c| format!("`{}`", c.path.display())).collect();
                format!("*Changed {}: {}*", plural(changes.len(), "file"), names.join(", "))
            }
            Item::Error(text) => format!("**Error:** {}", text.trim()),
            Item::Images(paths) => {
                let names: Vec<String> = paths
                    .iter()
                    .map(|p| p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned()))
                    .collect();
                format!("*[{} attached: {}]*", plural(paths.len(), "image"), names.join(", "))
            }
            Item::Earlier { messages, .. } => format!("*[{} earlier not loaded]*", plural(*messages, "message")),
            Item::Compacted { trigger, tokens, .. } => {
                let mut facts: Vec<String> = trigger.iter().cloned().collect();
                if let Some(t) = tokens {
                    facts.push(format!("{}k tokens", t / 1000));
                }
                let facts = if facts.is_empty() { String::new() } else { format!(" ({})", facts.join(", ")) };
                format!("---\n\n*Context compacted{facts}*\n\n---")
            }
            Item::AgentMessage { title, text, .. } => {
                let quoted: Vec<String> =
                    text.trim().lines().map(|l| if l.is_empty() { ">".to_string() } else { format!("> {l}") }).collect();
                format!("> **Report from {}**\n>\n{}", title.trim(), quoted.join("\n"))
            }
        };
        let tool = matches!(item, Item::Tool(_) | Item::Permission(_));
        if !out.is_empty() {
            // Tool calls in a row make one list.
            out.push_str(if tool && last_tool { "\n" } else { "\n\n" });
        }
        out.push_str(&block);
        last_tool = tool;
    }
    out.push('\n');
    out
}

/// A tool call as one list line: name, summary, change counts, how it ended.
fn tool_line(t: &ToolCall) -> String {
    let mut line = format!("- **{}**", t.name);
    let summary = one_line(&t.summary, 120);
    if !summary.is_empty() {
        line.push(' ');
        line.push_str(&summary);
    }
    if let Some(d) = &t.diff {
        line.push_str(&format!(" (+{} -{})", d.added(), d.removed()));
    }
    match &t.result {
        None if t.running() => line.push_str(" (running)"),
        None => line.push_str(" (did not finish)"),
        Some(r) if t.is_error => line.push_str(&format!(" -> error: {}", one_line(r, 100))),
        // Diffs say it all already.
        Some(_) if t.diff.is_some() => {}
        Some(r) if r.trim().is_empty() => {}
        Some(r) => line.push_str(&format!(" -> {}", one_line(r.trim(), 80))),
    }
    line
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 { format!("1 {word}") } else { format!("{n} {word}s") }
}

/// A code block that is one shell command written as for the agent's shell (`! git push`): the
/// command, without the `!`.
pub fn shell_command(code: &str) -> Option<String> {
    let mut lines = code.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next()?;
    let command = first.strip_prefix('!')?.trim();
    (lines.next().is_none() && !command.is_empty()).then(|| command.to_string())
}

/// How many tool calls in a row the chat shows as one group.
pub const TOOL_RUN: usize = 2;

/// Whether a call can be folded into a group at all: a failed call, an edit with its diff and a
/// call that returned images stay in view (what went wrong, the work itself, what it saw).
pub fn foldable(t: &ToolCall) -> bool {
    !t.is_error && t.diff.is_none() && t.images.is_empty()
}

/// The run of tool calls item `ix` belongs to, when it is in one: [`TOOL_RUN`] or more
/// [`Item::Tool`]s in a row, of any tools, each one [`foldable`] and `groupable` (a
/// subagent's card or a call with a fix to offer stays on its own). The agent's text between
/// them is what ends a run.
pub fn tool_run(items: &[Item], ix: usize, groupable: impl Fn(&ToolCall) -> bool) -> Option<std::ops::Range<usize>> {
    let member = |i: &Item| matches!(i, Item::Tool(t) if foldable(t) && groupable(t));
    if !member(items.get(ix)?) {
        return None;
    }
    let start = items[..ix].iter().rposition(|i| !member(i)).map_or(0, |p| p + 1);
    let end = items[ix..].iter().position(|i| !member(i)).map_or(items.len(), |p| ix + p);
    (end - start >= TOOL_RUN).then_some(start..end)
}

/// What a group of calls is called, tool by tool in the order they first ran:
/// "3 commands, 2 reads, 1 search".
pub fn tool_run_label<'a>(tools: impl IntoIterator<Item = &'a str>) -> String {
    let mut counts: Vec<(&'static str, &'static str, String, usize)> = Vec::new();
    for tool in tools {
        let (one, many): (&'static str, &'static str) = match tool {
            "Bash" | "Shell" | "BashOutput" => ("command", "commands"),
            "Read" => ("read", "reads"),
            "Edit" | "MultiEdit" | "Write" | "StrReplace" => ("edit", "edits"),
            "Grep" | "Glob" | "LS" => ("search", "searches"),
            "WebFetch" => ("fetch", "fetches"),
            "WebSearch" => ("web search", "web searches"),
            "TodoWrite" => ("to-do update", "to-do updates"),
            _ => ("", ""),
        };
        // Tools without a word of their own count under their name.
        let key = if one.is_empty() { tool.to_string() } else { one.to_string() };
        match counts.iter_mut().find(|c| c.2 == key) {
            Some(c) => c.3 += 1,
            None => counts.push((one, many, key, 1)),
        }
    }
    counts
        .iter()
        .map(|(one, many, name, n)| match (one.is_empty(), *n) {
            (false, 1) => format!("1 {one}"),
            (false, n) => format!("{n} {many}"),
            (true, 1) => format!("1 {name} call"),
            (true, n) => format!("{n} {name} calls"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod image_order_tests {
    use super::*;

    #[test]
    fn images_go_above_their_message() {
        let mut chat = Conversation::new("/tmp");
        chat.push_user("look at this");
        chat.apply(Event::UserImages(vec![PathBuf::from("/tmp/a.png")]));
        assert!(matches!(chat.items[0], Item::Images(_)));
        assert!(matches!(&chat.items[1], Item::User(t) if t == "look at this"));
        // A message of images alone (no text) follows the reply before it.
        chat.apply(Event::Text { text: "nice".into(), parent: None });
        chat.apply(Event::UserImages(vec![PathBuf::from("/tmp/b.png")]));
        assert!(matches!(chat.items.last(), Some(Item::Images(_))));
    }
}

#[cfg(test)]
mod tool_run_tests {
    use super::*;

    fn tool(id: &str, name: &str) -> Item {
        Item::Tool(ToolCall {
            id: id.into(),
            name: name.into(),
            input: Value::Null,
            summary: String::new(),
            result: None,
            is_error: false,
            diff: None,
            parent: None,
            started: None,
            duration_ms: None,
            live: None,
            issue: None,
            images: Vec::new(),
        })
    }

    #[test]
    fn calls_in_a_row_make_a_group_whatever_the_tool() {
        let mut failed = tool("x", "Bash");
        if let Item::Tool(t) = &mut failed {
            t.is_error = true;
        }
        let items = vec![
            Item::User("go".into()),
            tool("a", "Bash"),
            tool("b", "Read"),
            tool("c", "Bash"),
            Item::Assistant { text: "so".into(), streaming: false },
            tool("d", "Bash"),
            failed,
            tool("e", "Bash"),
            tool("f", "Grep"),
        ];
        let all = |_: &ToolCall| true;
        for ix in 1..=3 {
            assert_eq!(tool_run(&items, ix, all), Some(1..4));
        }
        // Text ends a run; a failed call stays out and splits it; one alone is no group.
        assert_eq!(tool_run(&items, 5, all), None);
        assert_eq!(tool_run(&items, 6, all), None);
        assert_eq!(tool_run(&items, 7, all), Some(7..9));
        assert_eq!(tool_run(&items, 0, all), None);
        // A call that is not groupable splits the run.
        assert_eq!(tool_run(&items, 1, |t: &ToolCall| t.id != "b"), None);
        assert_eq!(tool_run_label(["Bash", "Read", "Bash", "Grep"]), "2 commands, 1 read, 1 search");
        assert_eq!(tool_run_label(["Frobnicate", "Frobnicate"]), "2 Frobnicate calls");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_preview_gives_way_to_the_finished_reply() {
        let mut c = Conversation::new("/p");
        c.apply(Event::UserText("Explain".into()));
        c.apply(Event::TextPreview("How a hash".into()));
        c.apply(Event::TextPreview("How a hash map works".into()));
        assert_eq!(c.preview_item(), Some(1));
        assert!(matches!(&c.items[1], Item::Assistant { text, streaming: true } if text == "How a hash map works"));
        c.apply(Event::Text { text: "# How a hash map works\n\nIt hashes.".into(), parent: None });
        assert_eq!(c.preview_item(), None);
        assert_eq!(c.items.len(), 2);
        assert!(matches!(&c.items[1], Item::Assistant { text, streaming: false } if text.starts_with("# How")));
        // The screen still shows that reply: no second copy.
        c.apply(Event::TextPreview("How a hash map works It hashes.".into()));
        assert_eq!(c.items.len(), 2);
    }

    #[test]
    fn a_preview_nothing_replaced_goes() {
        let mut c = Conversation::new("/p");
        c.apply(Event::UserText("Go".into()));
        c.apply(Event::TextPreview("Listing the folder".into()));
        c.apply(Event::ToolCall { id: "t".into(), name: "Bash".into(), summary: "ls".into(), input: Value::Null, diff: None, parent: None });
        assert!(c.items.iter().all(|i| !matches!(i, Item::Assistant { .. })), "{:?}", c.items);
        assert_eq!(c.preview_item(), None);
    }

    use super::*;
    use crate::diff::{self, FileDiff};
    use serde_json::json;

    fn edit(id: &str, path: &str, old: &str, new: &str, parent: Option<&str>) -> [Event; 2] {
        let input = json!({"file_path": path, "old_string": old, "new_string": new});
        let d: Option<FileDiff> = diff::from_input("Edit", &input);
        let parent = parent.map(str::to_string);
        [
            Event::ToolCall { id: id.into(), name: "Edit".into(), summary: String::new(), input, diff: d.clone(), parent: parent.clone() },
            Event::ToolResult { id: id.into(), output: "ok".into(), is_error: false, diff: d, file_changed: Some(path.into()), parent },
        ]
    }

    #[test]
    fn file_changes_in_order_with_subagents() {
        let mut chat = Conversation::new("/p");
        chat.apply_all(edit("t1", "/p/a", "1", "2", None));
        chat.apply_all(edit("t2", "/p/b", "x", "y", None));
        chat.apply_all([
            Event::ToolCall { id: "ag".into(), name: "Agent".into(), summary: String::new(), input: json!({}), diff: None, parent: None },
            Event::SubagentStarted { id: "ag".into(), description: "help".into(), kind: "general".into() },
        ]);
        chat.apply_all(edit("t3", "/p/a", "2", "3", Some("ag")));
        // A call still running, and one that failed, changed nothing yet.
        chat.apply(edit("t4", "/p/a", "3", "4", None)[0].clone());
        let [call, _] = edit("t5", "/p/a", "3", "5", None);
        chat.apply_all([call, Event::ToolResult { id: "t5".into(), output: "no".into(), is_error: true, diff: None, file_changed: None, parent: None }]);

        let changes = chat.file_changes(Path::new("/p/a"));
        assert_eq!(changes.len(), 2);
        assert_eq!(diff::undo("3", &changes), "1");
        assert!(chat.edited(Path::new("/p/b")));
        assert!(!chat.edited(Path::new("/p/c")));
    }

    #[test]
    fn a_turn_and_the_session_list_their_changed_files() {
        let mut chat = Conversation::new("/p");
        chat.apply(Event::UserText("first".into()));
        chat.apply_all(edit("t1", "/p/src/a.rs", "1", "2", None));
        chat.apply_all(edit("t2", "/p/Cargo.lock", "x", "y", None));
        chat.apply(Event::UserText("second".into()));
        chat.apply_all(edit("t3", "/p/tests/a.rs", "1", "2", None));
        chat.apply_all([
            Event::ToolCall { id: "ag".into(), name: "Agent".into(), summary: String::new(), input: json!({}), diff: None, parent: None },
            Event::SubagentStarted { id: "ag".into(), description: "help".into(), kind: "general".into() },
        ]);
        chat.apply_all(edit("t4", "/p/src/a.rs", "2", "3", Some("ag")));
        chat.apply_all(edit("t5", "/p/src/a.rs", "3", "4", None));

        let paths = |c: Vec<crate::changes::FileChange>| c.into_iter().map(|c| (c.path.display().to_string(), c.edits)).collect::<Vec<_>>();
        // This turn: the subagent's edit counts, code before tests.
        assert_eq!(paths(chat.changes(chat.turn_start())), [("/p/src/a.rs".into(), 2), ("/p/tests/a.rs".into(), 1)]);
        // The session: the lock file last.
        assert_eq!(
            paths(chat.changes(0)),
            [("/p/src/a.rs".into(), 3), ("/p/tests/a.rs".into(), 1), ("/p/Cargo.lock".into(), 1)]
        );
    }

    #[test]
    fn a_turn_that_changed_files_ends_with_one_card() {
        let end = || Event::TurnEnded {
            error: None,
            cost_total_usd: None,
            input_tokens: 0,
            output_tokens: 0,
            duration_ms: None,
            session_id: None,
        };
        let mut chat = Conversation::new("/p");
        chat.apply(Event::UserText("look".into()));
        chat.apply(end());
        assert!(!chat.items.iter().any(|i| matches!(i, Item::TurnChanges(_))), "nothing changed, no card");
        chat.apply(Event::UserText("fix".into()));
        chat.apply_all(edit("t1", "/p/a.rs", "1", "2", None));
        chat.apply(end());
        // The same turn end again (hook, then the session file): still one card.
        chat.apply(end());
        let cards: Vec<_> = chat.items.iter().filter_map(|i| if let Item::TurnChanges(c) = i { Some(c) } else { None }).collect();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0][0].path, Path::new("/p/a.rs"));

        // What a command changed joins the card (once), and the turn's and session's lists.
        let turn = chat.turn_start();
        let cmd = |p: &str| crate::changes::FileChange {
            path: p.into(),
            added: 3,
            removed: 1,
            created: false,
            edits: 0,
            group: crate::changes::group_of(Path::new(p)),
            by_command: true,
            diff: None,
        };
        chat.add_command_changes(turn, vec![cmd("/p/fmt.rs")]);
        chat.add_command_changes(turn, vec![cmd("/p/fmt.rs")]);
        assert_eq!(chat.changes(turn).iter().map(|c| (c.path.display().to_string(), c.by_command)).collect::<Vec<_>>(), [
            ("/p/a.rs".into(), false),
            ("/p/fmt.rs".into(), true)
        ]);
        assert_eq!(chat.edited_in_turn(turn), [PathBuf::from("/p/a.rs")].into());
        // Too late: another turn has begun.
        chat.apply(Event::UserText("next".into()));
        chat.add_command_changes(turn, vec![cmd("/p/late.rs")]);
        assert!(!chat.changes(0).iter().any(|c| c.path == Path::new("/p/late.rs")));
    }

    fn call(id: &str, name: &str, summary: &str) -> Event {
        Event::ToolCall { id: id.into(), name: name.into(), summary: summary.into(), input: json!({}), diff: None, parent: None }
    }

    fn result(id: &str, output: &str, is_error: bool) -> Event {
        Event::ToolResult { id: id.into(), output: output.into(), is_error, diff: None, file_changed: None, parent: None }
    }

    #[test]
    fn tool_clocks_run_live_and_stop() {
        let mut chat = Conversation::new("/p");
        chat.apply_all([call("t1", "Bash", "ls"), call("t2", "Bash", "sleep 9")]);
        assert_eq!(chat.running_tools().count(), 2);
        assert!(chat.tool("t1").unwrap().elapsed().is_some());
        chat.apply(result("t1", "a\nb", false));
        let t1 = chat.tool("t1").unwrap();
        assert!(!t1.running() && t1.duration_ms.is_some());
        assert_eq!(chat.running_tools().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["t2"]);
        // The turn ended without its result: interrupted, its clock stops.
        chat.apply(Event::TurnEnded {
            error: None,
            cost_total_usd: None,
            input_tokens: 0,
            output_tokens: 0,
            duration_ms: None,
            session_id: None,
        });
        assert_eq!(chat.running_tools().count(), 0);
        assert!(chat.tool("t2").unwrap().elapsed().is_some());

        // Replayed history has no clocks.
        let mut replay = Conversation::new("/p");
        replay.replaying = true;
        replay.apply(call("t1", "Bash", "ls"));
        assert_eq!(replay.running_tools().count(), 0);
        assert_eq!(replay.tool("t1").unwrap().elapsed(), None);
    }

    #[test]
    fn markdown_of_a_conversation() {
        let mut chat = Conversation::new("/p");
        chat.items.push(Item::Earlier { from: 0, to: 10, messages: 3 });
        chat.push_user("Fix the build");
        chat.apply(Event::Text { text: "Looking.".into(), parent: None });
        chat.apply_all([call("t1", "Bash", "cargo build"), result("t1", "error[E0425]: oops\nmore", true)]);
        chat.apply_all(edit("t2", "/p/a.rs", "x", "y", None));
        chat.apply(call("t3", "Read", "b.rs"));
        chat.items.push(Item::Compacted { summary: "long".into(), trigger: Some("auto".into()), tokens: Some(120_000) });
        chat.items.push(Item::AgentMessage { from: "a1".into(), agent: None, title: "Survey".into(), text: "Found it.\n\nDone.".into() });
        chat.push_notice("Turn took 3s");

        let md = to_markdown(&chat.items, "Agent");
        assert_eq!(
            md,
            "*[3 messages earlier not loaded]*\n\n\
             ### You\n\nFix the build\n\n\
             ### Agent\n\nLooking.\n\n\
             - **Bash** cargo build -> error: error[E0425]: oops ...\n\
             - **Edit** (+1 -1)\n\
             - **Read** b.rs (running)\n\n\
             ---\n\n*Context compacted (auto, 120k tokens)*\n\n---\n\n\
             > **Report from Survey**\n>\n> Found it.\n>\n> Done.\n\n\
             *Turn took 3s*\n"
        );
    }
}

/// Letters and digits only, lower case: a reply as Markdown and as the screen shows it compare
/// equal this way.
fn letters(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

#[cfg(test)]
mod shell_command_tests {
    use super::shell_command;

    #[test]
    fn one_bang_line_is_a_command() {
        assert_eq!(shell_command("! git push -u origin main\n").as_deref(), Some("git push -u origin main"));
        assert_eq!(shell_command("!ls").as_deref(), Some("ls"));
        assert_eq!(shell_command("ls -la"), None);
        assert_eq!(shell_command("! a\n! b"), None, "several commands: copy them instead");
        assert_eq!(shell_command("!"), None);
    }
}
