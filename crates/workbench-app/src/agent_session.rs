//! One chat session: an agent (from its backend) and the conversation it feeds. No drawing here.
//!
//! For a backend with its own terminal interface (Claude Code), the session runs that interface
//! ONCE, in a pseudo-terminal, for the session's whole life. The Terminal view shows it; the
//! Chat view draws the same conversation from the session's file (followed as it grows) and its
//! message box types into that same process. Switching views only changes what is shown. The
//! interface reports through hooks (see `hooks.rs`): when it is up, when a message went in, each
//! tool, each permission request (answered from Chat, or in the terminal, whichever comes
//! first) and the end of each turn.
//!
//! A backend without such an interface runs its structured agent behind the window, and there
//! is only Chat.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::prelude::*;
use gpui_kit::*;
use workbench_core::agent::{Agent, Backend, Event, Follow, Hook, Message, StartOptions};
use workbench_core::conversation::Conversation;
use workbench_core::host::Host;

use crate::hooks::{self, Registration, Reply};
use crate::terminal_view::{Placeholder, TerminalView};

/// Something the rest of the window should react to.
#[derive(Clone, Debug)]
pub enum SessionEvent {
    /// The agent wrote or edited this file.
    FileChanged(PathBuf),
    TurnEnded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Chat,
    Terminal,
}

impl View {
    pub fn name(self) -> &'static str {
        match self {
            View::Chat => "chat",
            View::Terminal => "terminal",
        }
    }
}

/// The agent's own interface in its pseudo-terminal.
struct TerminalRun {
    view: Entity<TerminalView>,
    _exit: Subscription,
}

/// The session file being followed into the chat.
struct Following {
    _follow: Option<Follow>,
    _reader: Task<()>,
}

pub struct AgentSession {
    host: Arc<dyn Host>,
    pub cwd: PathBuf,
    pub chat: Conversation,
    pub backend: Arc<dyn Backend>,
    /// The structured agent (backends without a terminal interface).
    agent: Box<dyn Agent>,
    /// `None` is the backend's default mode (it asks); otherwise its own mode name.
    pub permission_mode: Option<String>,
    pub model: Option<String>,
    /// Something happened since the user last looked at this session.
    pub unseen: bool,
    /// The next start forks the resumed session.
    fork_next: bool,
    /// Waiting for an agent process elsewhere (this pid) to exit before resuming here.
    pub waiting_for: Option<u32>,
    _reader: Option<Task<()>>,
    pub view: View,
    terminal: Option<TerminalRun>,
    following: Option<Following>,
    hooks: Option<Registration>,
    /// The interface said it is up; messages typed before wait in `queued`.
    ready: bool,
    queued: Vec<String>,
    /// Messages shown here already whose copy in the session file is still to come.
    echo: VecDeque<String>,
    /// Permission hooks waiting for an answer, by request id.
    waiting: HashMap<String, Reply>,
    /// Recent tool calls (id, tool, input), to give a permission request its tool call's id.
    starting: VecDeque<(String, String, serde_json::Value)>,
    asked: u64,
    /// With `WORKBENCH_PERF=1`: when the last message was sent, until its reply shows.
    sent_at: Option<std::time::Instant>,
}

impl EventEmitter<SessionEvent> for AgentSession {}

impl AgentSession {
    pub fn new(host: Arc<dyn Host>, backend: Arc<dyn Backend>, cwd: PathBuf) -> Self {
        Self {
            agent: backend.new_agent(),
            backend,
            host,
            chat: Conversation::new(cwd.to_string_lossy().to_string()),
            cwd,
            permission_mode: std::env::var("WORKBENCH_PERMISSION_MODE").ok().filter(|m| !m.is_empty()),
            model: std::env::var("WORKBENCH_MODEL").ok().filter(|m| !m.is_empty()),
            unseen: false,
            fork_next: false,
            waiting_for: None,
            _reader: None,
            view: View::Chat,
            terminal: None,
            following: None,
            hooks: None,
            ready: false,
            queued: Vec::new(),
            echo: VecDeque::new(),
            waiting: HashMap::new(),
            starting: VecDeque::new(),
            asked: 0,
            sent_at: None,
        }
    }

    /// A session continuing an earlier one, its history already rebuilt from the file.
    /// `fork`: continue in a copy (the original runs elsewhere).
    pub fn resumed(host: Arc<dyn Host>, backend: Arc<dyn Backend>, cwd: PathBuf, chat: Conversation, fork: bool) -> Self {
        let mut s = Self::new(host, backend, cwd);
        s.chat = chat;
        s.chat.process_ended();
        s.fork_next = fork;
        s.chat.push_notice(if fork {
            "Forked: this continues a copy of the conversation here; the original keeps running."
        } else {
            "Resumed: the next message continues this conversation."
        });
        s
    }

    /// Whether the agent's own terminal interface drives this session (Chat is then a view of
    /// its session file). Local folders only: its hooks reach the window over a local socket.
    pub fn interactive(&self) -> bool {
        self.backend.capabilities().terminal_view && !self.host.is_remote()
    }

    /// Start the session: its agent's interface (unless it waits for another process), in the
    /// view it was last shown in, else the default for new sessions.
    pub fn begin(&mut self, cx: &mut Context<Self>) {
        if !self.interactive() {
            return;
        }
        let want = cx.global::<crate::AppSettings>().0.borrow().view_for(self.chat.session_id.as_deref()).to_string();
        if want == "terminal" {
            self.view = View::Terminal;
        }
        if self.waiting_for.is_none() {
            self.start_terminal(cx);
        }
    }

    /// The process elsewhere has exited: this session's own can start.
    pub fn takeover_done(&mut self, cx: &mut Context<Self>) {
        self.waiting_for = None;
        if self.interactive() && self.terminal.is_none() {
            self.start_terminal(cx);
        }
        cx.notify();
    }

    // ----- the agent's own interface -----

    fn running(&self, cx: &App) -> bool {
        self.terminal.as_ref().is_some_and(|t| !t.view.read(cx).exited)
    }

    /// Start the interface on this conversation (a new one, a resumed one, or a fork of one).
    fn start_terminal(&mut self, cx: &mut Context<Self>) {
        let started = std::time::Instant::now();
        if self.hooks.is_none() {
            self.hooks = hooks::register(cx.entity().downgrade(), self.backend.as_ref(), cx);
        }
        let fork = std::mem::take(&mut self.fork_next);
        let (id, new) = match self.chat.session_id.clone() {
            Some(id) => (id, false),
            None => (new_session_id(), true),
        };
        let options = StartOptions {
            cwd: self.cwd.clone(),
            model: self.model.clone(),
            permission_mode: self.permission_mode.clone(),
            resume: None,
            fork,
            settings: self.hooks.as_ref().map(|h| h.settings.clone()),
        };
        let Some(spec) = self.backend.terminal_program(&options, &id, new) else { return };
        let command = self.host.program_command(&spec);
        let placeholder = Placeholder {
            lines: last_lines(&self.chat, 12),
            label: format!("Starting {}...", self.backend.name()),
        };
        let view = cx.new(|cx| {
            let mut v = TerminalView::new(command, cx);
            v.placeholder = Some(placeholder);
            v
        });
        let exit = cx.observe(&view, |s, view, cx| {
            if view.read(cx).exited && s.ready {
                s.ready = false;
                s.drop_waiting(cx);
                s.chat.process_ended();
                s.chat.push_notice(format!(
                    "{} exited. The next message starts it again on this conversation.",
                    s.backend.name()
                ));
                cx.notify();
            } else if !s.ready && s.hooks.is_none() && view.read(cx).drawn {
                // No hooks to say when it is up: the first screen will do.
                s.set_ready(cx);
            }
        });
        self.ready = false;
        self.terminal = Some(TerminalRun { view, _exit: exit });
        if fork {
            // The copy's id comes with the interface's start (`Hook::Started`).
            self.following = None;
        } else {
            self.chat.session_id = Some(id.clone());
            self.follow(&id, cx);
        }
        if crate::perf::enabled() {
            eprintln!("workbench: agent interface started in {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
        }
        cx.notify();
    }

    /// Follow session `id`'s file into the chat from now on.
    fn follow(&mut self, id: &str, cx: &mut Context<Self>) {
        let (tx, rx) = async_channel::unbounded::<Message>();
        let follow = self.backend.follow_session(
            self.host.clone(),
            &self.cwd,
            id,
            Arc::new(move |m| {
                let _ = tx.send_blocking(m);
            }),
        );
        self.following = Some(Following { _follow: follow, _reader: Self::listen(rx, cx) });
    }

    fn set_ready(&mut self, cx: &mut Context<Self>) {
        if self.ready {
            return;
        }
        self.ready = true;
        for text in std::mem::take(&mut self.queued) {
            self.type_message(&text, cx);
        }
    }

    /// Type `text` into the interface as the person would: pasted, then Enter.
    fn type_message(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(run) = &self.terminal else { return };
        let Some(pty) = run.view.read(cx).pty.clone() else { return };
        let bracketed = run.view.read(cx).bracketed_paste();
        let mut pieces = self.backend.terminal_message(text, bracketed).into_iter();
        if let Some(first) = pieces.next() {
            pty.write(first);
        }
        let rest: Vec<Vec<u8>> = pieces.collect();
        if rest.is_empty() {
            return;
        }
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            for piece in rest {
                // A separate read for each piece, so the paste ends before Enter arrives.
                cx.background_executor().timer(std::time::Duration::from_millis(30)).await;
                pty.write(piece);
            }
        })
        .detach();
    }

    /// End every waiting permission hook without an answer: the person answers (or answered)
    /// in the terminal.
    fn drop_waiting(&mut self, _cx: &mut Context<Self>) {
        for (id, reply) in self.waiting.drain() {
            drop(reply);
            self.chat.answer_permission(&id, true);
        }
    }

    /// A hook call from the interface.
    pub fn on_hook(&mut self, call: hooks::Call, cx: &mut Context<Self>) {
        let Some(hook) = self.backend.parse_hook(&call.event, &call.payload, &self.chat.cwd) else { return };
        match hook {
            Hook::Started { session_id, .. } => {
                if let Some(id) = session_id
                    && self.chat.session_id.as_deref() != Some(id.as_str())
                {
                    // A fork (or a cleared conversation): follow the new file.
                    self.chat.session_id = Some(id.clone());
                    self.follow(&id, cx);
                }
                self.remember_view(cx);
                self.set_ready(cx);
            }
            Hook::Prompt(text) => {
                let t = text.trim().to_string();
                if !self.echo.iter().any(|e| *e == t) && !t.is_empty() {
                    // Typed in the terminal: show it now; the file's copy is skipped.
                    self.chat.push_user(&t);
                    self.push_echo(t);
                }
                self.chat.busy = true;
                self.chat.status = Some("working".into());
            }
            Hook::ToolStarting { id, tool, input } => {
                self.chat.busy = true;
                self.chat.status = Some(tool.clone());
                if let Some(id) = id {
                    self.starting.push_back((id, tool, input));
                    if self.starting.len() > 16 {
                        self.starting.pop_front();
                    }
                }
            }
            Hook::Permission { tool, summary, description, input } => {
                let request_id = self
                    .starting
                    .iter()
                    .rev()
                    .find(|(_, t, i)| *t == tool && *i == input)
                    .map(|(id, ..)| id.clone())
                    .unwrap_or_else(|| {
                        self.asked += 1;
                        format!("ask-{}", self.asked)
                    });
                self.chat.apply(Event::Permission { request_id: request_id.clone(), tool, summary, description, input });
                if let Some(reply) = call.reply {
                    self.waiting.insert(request_id, reply);
                }
                self.unseen = true;
            }
            Hook::ToolFinished { id, .. } => {
                // Answered in the terminal: the hook's wait ends without an answer.
                if let Some(id) = id
                    && self.waiting.remove(&id).is_some()
                {
                    self.chat.answer_permission(&id, true);
                }
            }
            Hook::TurnEnded => {
                self.drop_waiting(cx);
                let was_busy = self.chat.busy;
                self.chat.busy = false;
                self.chat.status = None;
                self.unseen = true;
                if was_busy {
                    cx.emit(SessionEvent::TurnEnded);
                }
            }
            Hook::Attention(_) => self.unseen = true,
        }
        self.chat.revision += 1;
        cx.notify();
    }

    fn push_echo(&mut self, text: String) {
        self.echo.push_back(text);
        if self.echo.len() > 8 {
            self.echo.pop_front();
        }
    }

    // ----- the structured agent (backends without a terminal interface) -----

    fn start(&mut self, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let options = StartOptions {
            cwd: self.cwd.clone(),
            model: self.model.clone(),
            permission_mode: self.permission_mode.clone(),
            resume: self.chat.session_id.clone(),
            fork: std::mem::take(&mut self.fork_next),
            settings: None,
        };
        let (tx, rx) = async_channel::unbounded::<Message>();
        self.agent.start(
            self.host.as_ref(),
            &options,
            Arc::new(move |m| {
                let _ = tx.send_blocking(m);
            }),
        )?;
        self._reader = Some(Self::listen(rx, cx));
        Ok(())
    }

    /// Apply what arrives on `rx`, a batch per wake-up.
    fn listen(rx: async_channel::Receiver<Message>, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            while let Ok(first) = rx.recv().await {
                let mut batch = vec![first];
                while let Ok(more) = rx.try_recv() {
                    batch.push(more);
                }
                let ok = this.update(cx, |session, cx| {
                    for m in batch {
                        session.handle(m, cx);
                    }
                    session.unseen = true;
                    cx.notify();
                });
                if ok.is_err() {
                    break;
                }
            }
        })
    }

    fn handle(&mut self, message: Message, cx: &mut Context<Self>) {
        match message {
            Message::Event(event) => {
                if let Event::UserText(text) = &event
                    && let Some(i) = self.echo.iter().position(|e| e == text.trim())
                {
                    // Shown already, when it was sent.
                    self.echo.remove(i);
                    return;
                }
                if matches!(event, Event::Text { .. } | Event::TextDelta(_) | Event::ToolCall { .. })
                    && let Some(at) = self.sent_at.take()
                {
                    eprintln!("workbench: reply shows {:.0} ms after Send", at.elapsed().as_secs_f64() * 1000.0);
                }
                let applied = self.chat.apply(event);
                if let Some(path) = applied.file_changed {
                    cx.emit(SessionEvent::FileChanged(path));
                }
                if applied.turn_ended {
                    cx.emit(SessionEvent::TurnEnded);
                }
            }
            Message::Log(line) => {
                if !line.trim().is_empty() {
                    self.chat.push_notice(format!("{}: {line}", self.backend.name()));
                }
            }
            Message::Closed => {
                self.agent.stop();
                if self.chat.busy {
                    self.chat.push_error(format!("{} stopped before the turn finished.", self.backend.name()));
                }
                self.chat.process_ended();
            }
        }
    }

    // ----- what the window asks -----

    /// Send a user turn, starting the agent when needed.
    pub fn send(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if let Some(pid) = self.waiting_for {
            self.chat.push_error(format!(
                "This conversation is still open in another terminal (process {pid}). Quit it there first; it resumes here by itself."
            ));
            cx.notify();
            return;
        }
        if crate::perf::enabled() {
            self.sent_at = Some(std::time::Instant::now());
        }
        if self.interactive() {
            self.chat.push_user(text);
            self.push_echo(text.to_string());
            if !self.running(cx) {
                self.start_terminal(cx);
            }
            if self.ready {
                self.type_message(text, cx);
            } else {
                self.queued.push(text.to_string());
            }
            cx.notify();
            return;
        }
        if !self.agent.running()
            && let Err(e) = self.start(cx)
        {
            self.chat.push_error(e.to_string());
            cx.notify();
            return;
        }
        self.chat.push_user(text);
        if let Err(e) = self.agent.send(text) {
            self.chat.push_error(format!("Could not send to {}: {e}", self.backend.name()));
        }
        cx.notify();
    }

    pub fn answer(&mut self, request_id: &str, allow: bool, cx: &mut Context<Self>) {
        if let Some(reply) = self.waiting.remove(request_id) {
            reply.send(&self.backend.hook_reply(allow));
            self.chat.answer_permission(request_id, allow);
        } else if let Some(input) = self.chat.answer_permission(request_id, allow) {
            let _ = self.agent.answer_permission(request_id, allow, &input);
        }
        cx.notify();
    }

    /// Answer the oldest open permission prompt (for the control channel).
    pub fn answer_first(&mut self, allow: bool, cx: &mut Context<Self>) {
        let id = self.chat.pending_permissions().next().map(|p| p.request_id.clone());
        if let Some(id) = id {
            self.answer(&id, allow, cx);
        }
    }

    pub fn interrupt(&mut self, cx: &mut Context<Self>) {
        if self.interactive() {
            if let Some(pty) = self.terminal.as_ref().and_then(|t| t.view.read(cx).pty.clone()) {
                pty.write(self.backend.terminal_interrupt());
            }
        } else {
            let _ = self.agent.interrupt();
        }
        cx.notify();
    }

    /// Switch permission mode: the agent restarts on the same conversation, now if it is idle.
    pub fn set_permission_mode(&mut self, mode: Option<String>, cx: &mut Context<Self>) {
        if self.permission_mode == mode {
            return;
        }
        self.permission_mode = mode;
        let now = self.interactive() && self.running(cx) && !self.chat.busy;
        self.stop();
        if now {
            self.start_terminal(cx);
        }
        self.chat.push_notice(format!(
            "Permission mode: {}. {}",
            self.permission_mode.as_deref().unwrap_or("ask before acting"),
            if now { "Restarted with it." } else { "Applies from the next message." }
        ));
        cx.notify();
    }

    // ----- views -----

    pub fn terminal_view(&self) -> Option<Entity<TerminalView>> {
        self.terminal.as_ref().map(|t| t.view.clone())
    }

    /// Show Chat or Terminal. Nothing is started or stopped: both are views of one process.
    pub fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        if view == self.view || (view == View::Terminal && !self.interactive()) {
            return;
        }
        crate::perf::switch_begin(if view == View::Terminal { "to Terminal" } else { "to Chat" });
        if view == View::Terminal && self.terminal.is_none() && self.waiting_for.is_none() {
            self.start_terminal(cx);
        }
        self.view = view;
        self.remember_view(cx);
        cx.notify();
    }

    fn remember_view(&self, cx: &mut Context<Self>) {
        if let Some(id) = &self.chat.session_id {
            let settings = cx.global::<crate::AppSettings>().0.clone();
            settings.borrow_mut().set_view(id, self.view.name());
            let _ = settings.borrow().save();
        }
    }

    pub fn stop(&mut self) {
        self.terminal = None;
        self.following = None;
        self.ready = false;
        for (_, reply) in self.waiting.drain() {
            drop(reply);
        }
        self.agent.stop();
        self._reader = None;
        self.chat.process_ended();
    }
}

/// The conversation's last `n` lines of text, oldest first, `true` for the person's.
fn last_lines(chat: &Conversation, n: usize) -> Vec<(bool, String)> {
    use workbench_core::conversation::Item;
    let mut out = Vec::new();
    for item in chat.items.iter().rev() {
        let (user, text) = match item {
            Item::User(t) => (true, t),
            Item::Assistant { text, .. } => (false, text),
            _ => continue,
        };
        for line in text.lines().rev().filter(|l| !l.trim().is_empty()) {
            out.push((user, line.to_string()));
            if out.len() >= n {
                out.reverse();
                return out;
            }
        }
    }
    out.reverse();
    out
}

/// A random version 4 UUID, for a new session started in the agent's interface.
fn new_session_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = [0u8; 16];
    for chunk in bytes.chunks_mut(8) {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

impl Drop for AgentSession {
    fn drop(&mut self) {
        self.stop();
    }
}
