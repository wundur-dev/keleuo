//! One chat session: an agent (from its backend) and the conversation it feeds. No drawing here.
//!
//! For a backend with its own terminal interface (Claude Code), each view runs the agent its own
//! way, one at a time: Chat runs it headless (its structured stream, behind the window) and
//! Terminal runs its interface in a pseudo-terminal. Switching ends one and resumes the
//! conversation in the other, so it waits for the agent to be idle (see `switch_blocked`).
//!
//! With the experimental hybrid setting the session runs the interface ONCE, for its whole
//! life: Terminal shows it, and Chat draws the same conversation from the session's file
//! (followed as it grows) and types its messages into that same process, so switching only
//! changes what is shown. Whenever the interface runs it reports through hooks (see
//! `hooks.rs`): when it is up, when a message went in, each tool, each permission request
//! (answered from Chat, or in the terminal, whichever comes first) and the end of each turn.
//!
//! A backend without such an interface runs its structured agent behind the window, and there
//! is only Chat.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::prelude::*;
use gpui_kit::*;
use keleuo_core::summary;
use keleuo_core::agent::{Agent, Backend, Event, Follow, Hook, Launch, Message, ModelOption, StartOptions};
use keleuo_core::conversation::Conversation;
use keleuo_core::host::Host;
use keleuo_core::outbox::{Outbox, SendNow, Waiting};

use crate::hooks::{self, Registration, Reply};
use crate::terminal_view::{ImageSupport, PasteImage, Placeholder, TerminalEvent, TerminalView};
use std::rc::Rc;

/// Parts of a session read for a search: each part's items, by where the part starts.
pub type EarlierParts = HashMap<u64, Vec<keleuo_core::conversation::Item>>;

/// What a session has used, as the window shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum Spend {
    /// Dollars (`estimated`: from token counts and published prices).
    Cost { total: f64, turn: Option<f64>, estimated: bool },
    /// How full the context window is after the last turn.
    Context { percent: u32, tokens: u64, window: u64, fill: Fill },
}

/// Whether cost shows rather than the context, as the setting says and, on auto, for a session
/// billed per use (`per_use`).
pub fn shows_cost(per_use: Option<bool>, cx: &App) -> bool {
    match cx.global::<crate::AppSettings>().0.borrow().cost_display.as_str() {
        "always" => true,
        "never" => false,
        _ => per_use == Some(true),
    }
}

/// How close a session's context is to where its agent compacts it by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fill {
    Room,
    /// From 70% of the way to the compaction point.
    Filling,
    /// From 90%: the agent compacts soon.
    Nearly,
}

impl Fill {
    pub fn of(tokens: u64, compacts_at: u64) -> Self {
        match tokens * 100 / compacts_at.max(1) {
            90.. => Fill::Nearly,
            70.. => Fill::Filling,
            _ => Fill::Room,
        }
    }
}

/// How much of its context a session uses, after the last turn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContextUse {
    pub tokens: u64,
    pub window: u64,
    /// Where the agent compacts by itself (a little short of the window).
    pub compacts_at: u64,
}

impl ContextUse {
    pub fn fill(&self) -> Fill {
        Fill::of(self.tokens, self.compacts_at)
    }
}

impl Spend {
    /// Short, for the session list.
    pub fn short(&self) -> String {
        match self {
            Spend::Cost { total, estimated, .. } => {
                let est = if *estimated { "~" } else { "" };
                if *total < 0.1 { format!("{est}${total:.3}") } else { format!("{est}${total:.2}") }
            }
            Spend::Context { percent, .. } => format!("{percent}%"),
        }
    }
}

/// The least time between two batches of the agent's messages (see `listen`).
const BATCH_EVERY: std::time::Duration = std::time::Duration::from_millis(100);

/// Seconds to wait for the interface to say it is up before pointing to the terminal.
const ASK_AFTER_SECS: u64 = 6;

/// The agent's interface waits on a question Chat does not draw itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asking {
    /// "Do you trust the files in this folder?": answered from Chat with Trust or Don't trust.
    Trust,
    /// Something else (a login, an approval, a list to choose from): answered in the terminal.
    Other,
}

/// Something the rest of the window should react to.
#[derive(Clone, Debug)]
pub enum SessionEvent {
    /// The agent wrote or edited this file.
    FileChanged(PathBuf),
    TurnEnded,
    /// Cmd+click in the agent's terminal interface: the row's text and the column clicked.
    TerminalLink { line: String, column: usize },
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
    _links: Subscription,
}

/// The session file being followed into the chat.
struct Following {
    _follow: Option<Follow>,
    _reader: Task<()>,
}

/// The issue a write refused by the sandbox raises (Keleuo's own, not the agent's).
pub(crate) const SANDBOX_ISSUE: &str = "keleuo-sandbox";

pub struct AgentSession {
    host: Arc<dyn Host>,
    /// Opened again at start: its conversation shows, but its agent starts only when it is
    /// needed (a message, or its Terminal shown), so reopening many sessions costs nothing.
    pub dormant: bool,
    /// When the reply preview was last read off the terminal (at most ten times a second).
    /// Reads the journals of the workflows running (see `follow_workflows`).
    workflow_poll: Option<Task<()>>,
    preview_read: Option<std::time::Instant>,
    /// The preview last shown (this turn's): the same words again change nothing.
    previewed: Option<String>,
    /// Messages the interface reported submitted (its prompt hook), to tell a sent message
    /// from one whose Enter was lost.
    prompts_seen: u64,
    pub cwd: PathBuf,
    pub chat: Conversation,
    pub backend: Arc<dyn Backend>,
    /// Every model the agent lists for this person (see [`Backend::list_models`]); `None` until
    /// asked ([`AgentSession::want_models`]).
    pub listed_models: Option<Vec<ModelOption>>,
    /// The structured agent (backends without a terminal interface).
    agent: Box<dyn Agent>,
    /// `None` is the backend's default mode (it asks); otherwise its own mode name.
    pub permission_mode: Option<String>,
    pub model: Option<String>,
    /// The effort level, for an agent that takes one apart from the model; `None`: its default.
    pub effort: Option<String>,
    /// Something happened since the user last looked at this session.
    pub unseen: bool,
    /// The next start forks the resumed session.
    fork_next: bool,
    /// Its agent may write anywhere; else only in its folder and `writable` (see
    /// [`keleuo_core::sandbox`]). Kept with the session.
    pub writes_anywhere: bool,
    pub writable: Vec<PathBuf>,
    /// The agent last started with its writes kept to its folder.
    confined: bool,
    /// A reviewer: it reads and answers, its folder not writable (see `messaging`).
    pub read_only: bool,
    /// Messages from other sessions on their way to the agent: they go as turns, but show as
    /// the other session's card, not as the person's message.
    quiet: Vec<String>,
    /// Waiting for an agent process elsewhere (this pid) to exit before resuming here.
    pub waiting_for: Option<u32>,
    _reader: Option<Task<()>>,
    pub view: View,
    terminal: Option<TerminalRun>,
    following: Option<Following>,
    hooks: Option<Registration>,
    /// The interface said it is up; messages typed before wait in `queued`.
    ready: bool,
    queued: Vec<(String, Vec<PathBuf>)>,
    /// Messages shown here already whose copy in the session file is still to come.
    echo: VecDeque<String>,
    /// Permission hooks waiting for an answer, by request id.
    waiting: HashMap<String, Reply>,
    /// Recent tool calls (id, tool, input), to give a permission request its tool call's id.
    starting: VecDeque<(String, String, serde_json::Value)>,
    asked: u64,
    /// With `KELEUO_PERF=1`: when the last message was sent, until its reply shows.
    sent_at: Option<std::time::Instant>,
    /// Billed per use (an API key) rather than by a plan; `None` until known.
    pub per_use: Option<bool>,
    /// The interface waits on a question before it is up (see [`Asking`]).
    pub asking: Option<Asking>,
    /// The agent's own record of whether this folder is trusted, when known.
    trusted: Option<bool>,
    /// The permission mode the interface reports it is in (from its hooks).
    pub live_mode: Option<String>,
    /// The file's copy of a message shown when it was sent carries its images too: skip them.
    skip_images: usize,
    /// Images pasted into this run of the interface, in order: its `[Image #N]` is the Nth.
    pasted_images: Vec<PathBuf>,
    /// Restart the interface when the turn ends (a new model).
    restart_after_turn: bool,
    /// Why the interface is starting again, while it does (for the composer).
    pub restarting: Option<String>,
    /// The current turn (where it starts in the chat) and the git working tree when it started,
    /// once read: what commands changed is found against it when the turn ends.
    git_turn: Option<(usize, Option<keleuo_core::command_changes::Baseline>)>,
    /// The interface drives the session in both views (the experimental hybrid setting, read
    /// when the session begins).
    hybrid: bool,
    /// The interface is ending before Chat's agent works; that agent starts once it has.
    closing: bool,
    /// Messages sent while the interface ends, sent once it has.
    held: Vec<(String, Vec<PathBuf>)>,
    /// Messages sent while the agent works, each sent when the turn before it ends.
    outbox: Outbox,
    /// The person stopped the turn: what waits stays until they send it, or send another.
    outbox_paused: bool,
    /// Without the hybrid, the view not shown keeps its agent, idle, in case the conversation
    /// has not moved on when it shows again (then switching back restarts nothing): the
    /// interface, Terminal hidden...
    parked: Option<Parked>,
    /// ...or Chat's headless agent, Terminal shown, and how long the session file was then.
    agent_parked_at: Option<u64>,
    /// Counts switches, so a parked agent's time limit is its own park's.
    switches: u64,
    /// Turns whose summary was asked for (by key), so a turn's end seen twice asks once.
    summarized: HashSet<String>,
}

/// The interface, kept idle while Chat shows (see `parked`).
struct Parked {
    run: TerminalRun,
    pty: Option<Rc<crate::pty::Pty>>,
    /// The session file's length when it was parked (`None`: not known, so it cannot be kept).
    len: Option<u64>,
    ready: bool,
}

/// How long the view not shown keeps its idle agent.
const PARK_FOR: std::time::Duration = std::time::Duration::from_secs(600);

impl EventEmitter<SessionEvent> for AgentSession {}

impl AgentSession {
    /// Apply a fix for a known tool failure (see `Issue`) in this session's folder.
    pub fn apply_fix(&mut self, kind: &str, fix: &str, cx: &mut Context<Self>) -> std::io::Result<String> {
        if kind == SANDBOX_ISSUE {
            return Ok(match fix.strip_prefix("allow:") {
                Some(folder) => {
                    self.set_writable(PathBuf::from(folder), true, cx);
                    format!("Writes allowed in {folder} for this session.")
                }
                None => {
                    self.set_writes_anywhere(true, cx);
                    "Writes allowed anywhere for this session.".to_string()
                }
            });
        }
        self.backend.apply_fix(self.host.as_ref(), &self.cwd, kind, fix)
    }

    pub fn new(host: Arc<dyn Host>, backend: Arc<dyn Backend>, cwd: PathBuf) -> Self {
        Self {
            agent: backend.new_agent(),
            backend,
            host,
            dormant: false,
            workflow_poll: None,
            preview_read: None,
            previewed: None,
            prompts_seen: 0,
            listed_models: None,
            chat: Conversation::new(cwd.to_string_lossy().to_string()),
            cwd,
            permission_mode: keleuo_core::env_var("PERMISSION_MODE").ok().filter(|m| !m.is_empty()),
            model: keleuo_core::env_var("MODEL").ok().filter(|m| !m.is_empty()),
            effort: None,
            unseen: false,
            fork_next: false,
            writes_anywhere: false,
            writable: Vec::new(),
            confined: false,
            read_only: false,
            quiet: Vec::new(),
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
            per_use: None,
            asking: None,
            trusted: None,
            live_mode: None,
            skip_images: 0,
            pasted_images: Vec::new(),
            restart_after_turn: false,
            restarting: None,
            git_turn: None,
            hybrid: false,
            closing: false,
            held: Vec::new(),
            outbox: Outbox::default(),
            outbox_paused: false,
            parked: None,
            agent_parked_at: None,
            switches: 0,
            summarized: HashSet::new(),
        }
    }

    /// A session continuing an earlier one, its history already rebuilt from the file.
    /// `fork`: continue in a copy (the original runs elsewhere).
    pub fn resumed(host: Arc<dyn Host>, backend: Arc<dyn Backend>, cwd: PathBuf, chat: Conversation, fork: bool) -> Self {
        let mut s = Self::new(host, backend, cwd);
        s.chat = chat;
        s.attach_summaries();
        s.chat.process_ended();
        s.fork_next = fork;
        s.chat.push_notice(if fork {
            "Forked: this continues a copy of the conversation here; the original keeps running."
        } else {
            "Resumed: the next message continues this conversation."
        });
        s
    }

    /// Read an earlier part of this session's file (off the UI thread).
    pub fn earlier_loader(&self) -> Option<impl FnOnce(u64, u64) -> std::io::Result<Vec<keleuo_core::conversation::Item>> + Send + 'static> {
        let (host, backend, cwd) = (self.host.clone(), self.backend.clone(), self.cwd.clone());
        let id = self.chat.session_id.clone()?;
        Some(move |from, to| backend.load_segment(host.as_ref(), &cwd, &id, from, to))
    }

    /// Read several parts of the session not in the list (for a search): each part's items by
    /// where it starts; a part that cannot be read is left out.
    pub fn earlier_loader_many(&self) -> Option<impl FnOnce(Vec<(u64, u64)>) -> EarlierParts + Send + 'static> {
        let (host, backend, cwd) = (self.host.clone(), self.backend.clone(), self.cwd.clone());
        let id = self.chat.session_id.clone()?;
        Some(move |parts: Vec<(u64, u64)>| {
            parts
                .into_iter()
                .filter_map(|(from, to)| Some((from, backend.load_segment(host.as_ref(), &cwd, &id, from, to).ok()?)))
                .collect()
        })
    }

    /// Messages waiting for the agent to be up.
    pub fn queued_count(&self) -> usize {
        self.queued.len() + self.held.len()
    }

    /// Messages sent while the agent works, in the order they go.
    pub fn waiting(&self) -> impl Iterator<Item = &Waiting> {
        self.outbox.items()
    }

    /// Whether what waits stays until the person sends it (they stopped the turn).
    pub fn waiting_paused(&self) -> bool {
        self.outbox_paused && !self.outbox.is_empty()
    }

    /// Whether a message sent now goes into the running turn: the agent reads it as it works.
    pub fn takes_mid_turn(&self) -> bool {
        if self.interactive() { self.backend.terminal_takes_mid_turn() } else { self.agent.takes_mid_turn() }
    }

    /// Send waiting message `id` now: into the running turn when the agent takes it, else it
    /// goes first once the turn has been stopped.
    pub fn send_waiting_now(&mut self, id: u64, cx: &mut Context<Self>) {
        self.outbox_paused = false;
        match SendNow::decide(self.chat.busy, self.takes_mid_turn()) {
            SendNow::Now if self.chat.busy => {
                if let Some(w) = self.outbox.remove(id) {
                    self.deliver(&w.text, w.images, cx);
                }
            }
            SendNow::Now => {
                self.outbox.to_front(id);
                self.send_waiting(cx);
            }
            SendNow::StopFirst => {
                if self.outbox.to_front(id) {
                    self.stop_turn(cx);
                }
            }
        }
        cx.notify();
    }

    /// Drop waiting message `id`.
    pub fn remove_waiting(&mut self, id: u64, cx: &mut Context<Self>) {
        self.outbox.remove(id);
        cx.notify();
    }

    /// Send what waits while the agent is free: the first message, which starts a turn; the
    /// next waits for that turn to end.
    fn send_waiting(&mut self, cx: &mut Context<Self>) {
        while !self.chat.busy && !self.closing && !self.outbox_paused && self.waiting_for.is_none() {
            let Some(w) = self.outbox.next(self.chat.busy) else { break };
            self.deliver(&w.text, w.images, cx);
            cx.notify();
        }
    }

    /// Whether the agent's own terminal interface drives this session now: in Terminal, or in
    /// both views with the hybrid setting (Chat is then a view of its session file). On
    /// another machine too: its hooks reach the window through a socket its ssh connection
    /// forwards.
    pub fn interactive(&self) -> bool {
        self.backend.capabilities().terminal_view && (self.hybrid || self.view == View::Terminal)
    }

    /// Why the other view cannot be shown now, if it cannot. Switching restarts the agent in
    /// the other view, which would cut short what it is doing (not with the hybrid setting,
    /// where both views show one process).
    pub fn switch_blocked(&self) -> Option<String> {
        if self.hybrid || !self.backend.capabilities().terminal_view {
            return None;
        }
        let name = self.backend.name();
        if self.closing {
            Some(format!("{name} is closing in the terminal; one moment."))
        } else if self.chat.pending_permissions().next().is_some() {
            Some(format!("{name} is waiting for permission. Answer it first."))
        } else if self.chat.busy || self.chat.running_tools().next().is_some() {
            Some(format!("{name} is working. Switch when it's done."))
        } else if self.chat.running_agents().next().is_some() {
            Some("A subagent is still running in the background. Switch when it's done.".into())
        } else {
            None
        }
    }

    /// Start the session: its agent's interface (unless it waits for another process), in the
    /// view it was last shown in, else the default for new sessions.
    pub fn begin(&mut self, cx: &mut Context<Self>) {
        // How it is billed decides whether cost or context shows (settings files: off the UI
        // thread).
        let (host, backend, cwd) = (self.host.clone(), self.backend.clone(), self.cwd.clone());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let (per_use, trusted) = cx
                .background_spawn(async move { (backend.billed_per_use(host.as_ref(), &cwd), backend.folder_trusted(host.as_ref(), &cwd)) })
                .await;
            let _ = this.update(cx, |s, cx| {
                s.per_use = per_use;
                s.trusted = trusted;
                cx.notify();
            });
        })
        .detach();
        self.hybrid = crate::prefs(cx).hybrid_views;
        // Always a mode of its own, passed to the agent whichever way it runs (left to the
        // agent, its interface and its headless mode start in different ones).
        if self.permission_mode.is_none() {
            self.permission_mode = Some(cx.global::<crate::AppSettings>().0.borrow().mode_for(self.chat.session_id.as_deref()));
        }
        if !self.backend.capabilities().terminal_view {
            return;
        }
        let want = cx.global::<crate::AppSettings>().0.borrow().view_for(self.chat.session_id.as_deref()).to_string();
        if want == "terminal" {
            self.view = View::Terminal;
        }
        // Chat without the hybrid: its agent starts with the first message.
        if !self.interactive() {
            return;
        }
        if self.waiting_for.is_none() && !self.dormant {
            self.start_terminal(cx);
        }
    }

    /// The terminal drew: while the agent works and Chat shows, read the reply it is writing
    /// off the screen as a preview (the record has it only once it is finished).
    fn read_preview(&mut self, view: &Entity<TerminalView>, cx: &mut Context<Self>) {
        if !self.chat.busy {
            self.previewed = None;
            return;
        }
        if self.view != View::Chat {
            return;
        }
        let now = std::time::Instant::now();
        if self.preview_read.is_some_and(|t| now.duration_since(t) < PREVIEW_EVERY) {
            return;
        }
        self.preview_read = Some(now);
        let Some((lines, columns)) = view.read(cx).recent_lines(PREVIEW_LINES) else { return };
        // Claude Code redraws its screen many times a second; the chat only when the words do.
        if let Some(text) = self.backend.reply_preview(&lines, columns)
            && self.previewed.as_deref() != Some(text.as_str())
        {
            self.previewed = Some(text.clone());
            self.chat.apply(Event::TextPreview(text));
            cx.notify();
        }
    }

    /// A dormant session was shown: start its agent if its Terminal is what shows.
    pub fn wake(&mut self, cx: &mut Context<Self>) {
        if self.dormant && self.view == View::Terminal && self.interactive() && self.terminal.is_none() && self.waiting_for.is_none() {
            self.start_terminal(cx);
        }
    }

    /// The process elsewhere has exited: this session's own can start.
    pub fn takeover_done(&mut self, cx: &mut Context<Self>) {
        self.waiting_for = None;
        if self.interactive() && self.terminal.is_none() {
            self.start_terminal(cx);
        }
        self.send_waiting(cx);
        cx.notify();
    }

    /// Cost when it is billed per use (or the setting says always), else how full the context
    /// is. `None` before there is anything to show.
    pub fn spend(&self, cx: &App) -> Option<Spend> {
        self.spend_as(shows_cost(self.per_use, cx))
    }

    /// What it used, as cost (`cost`) or as how full its context is.
    pub fn spend_as(&self, cost: bool) -> Option<Spend> {
        if cost {
            return (self.chat.total_cost_usd > 0.0).then_some(Spend::Cost {
                total: self.chat.total_cost_usd,
                turn: self.chat.last_turn_cost_usd,
                estimated: self.chat.cost_estimated,
            });
        }
        let used = self.context_use()?;
        let ContextUse { tokens, window, .. } = used;
        Some(Spend::Context { percent: ((tokens * 100) / window.max(1)) as u32, tokens, window, fill: used.fill() })
    }

    /// How much of its context the session uses: the window as the agent reported it, else
    /// as Keleuo knows the model. `None` before a turn has ended, or when the agent says nothing.
    pub fn context_use(&self) -> Option<ContextUse> {
        let tokens = self.chat.last_turn_tokens?.0;
        let window = match self.chat.context_window {
            Some(w) => w,
            None => {
                let model = self.chat.model.clone().or_else(|| self.model.clone())?;
                self.backend.context_window(&model)?
            }
        };
        (tokens > 0 && window > 0).then(|| ContextUse { tokens, window, compacts_at: self.backend.compacts_at(window) })
    }

    /// Ask the agent to compact its context now, when it can be asked.
    pub fn compact(&mut self, cx: &mut Context<Self>) {
        if let Some(command) = self.backend.compact_command() {
            self.send(command, cx);
        }
    }

    // ----- questions before the interface is up -----

    /// Before the interface is up: is its screen a question Chat should point to?
    fn check_asking(&mut self, view: &Entity<TerminalView>, cx: &mut Context<Self>) {
        let screen = view.read(cx).screen_text();
        let asking = if screen.contains("trust this folder") || (self.trusted == Some(false) && screen.contains("Enter to confirm")) {
            Some(Asking::Trust)
        } else if screen.contains("Enter to confirm") || screen.contains("Esc to cancel") || screen.contains("Press Enter") {
            Some(Asking::Other)
        } else {
            self.asking.filter(|a| *a == Asking::Other)
        };
        if asking != self.asking {
            self.asking = asking;
            self.unseen = true;
            cx.notify();
        }
    }

    /// Answer the trust question as a person would: move the selection to the answer with the
    /// arrow keys, then Enter. Don't trust makes the interface exit.
    pub fn answer_trust(&mut self, trust: bool, cx: &mut Context<Self>) {
        let Some(view) = self.terminal_view() else { return };
        let target = if trust { "Yes, I trust" } else { "No, exit" };
        self.asking = None;
        cx.notify();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            for _ in 0..6 {
                let (pty, chosen, present) = view.update(cx, |v, _| {
                    let screen = v.screen_text();
                    let line = screen.lines().find(|l| l.contains(target)).map(str::to_string);
                    (v.pty.clone(), line.as_deref().is_some_and(|l| l.contains('❯')), line.is_some())
                });
                let Some(pty) = pty else { return };
                if !present {
                    break;
                }
                if chosen {
                    pty.write(b"\r".to_vec());
                    return;
                }
                pty.write(b"\x1b[B".to_vec());
                cx.background_executor().timer(std::time::Duration::from_millis(120)).await;
            }
            // Not found as expected: the person answers in the terminal.
            let _ = this.update(cx, |s, cx| {
                s.asking = Some(Asking::Other);
                cx.notify();
            });
        })
        .detach();
    }

    // ----- the agent's own interface -----

    fn running(&self, cx: &App) -> bool {
        self.terminal.as_ref().is_some_and(|t| !t.view.read(cx).exited)
    }

    /// Start the interface on this conversation (a new one, a resumed one, or a fork of one).
    /// Where the agent may write: its folder (and its own state, caches, temporary folders),
    /// when Settings keeps agents' writes there and this machine can; else `None` (anywhere).
    /// An agent on another machine runs unconfined.
    fn write_policy(&self, cx: &App) -> Option<keleuo_core::sandbox::WritePolicy> {
        if self.host.is_remote() {
            return None;
        }
        // A reviewer writes nowhere in its folder, whatever Settings says.
        if self.read_only {
            let mut extra = keleuo_core::mcp::writes(&self.mcp_servers(cx));
            extra.retain(|f| !f.starts_with(&self.cwd));
            return Some(keleuo_core::sandbox::read_only_policy(&self.cwd, &self.backend.state_paths(), &extra));
        }
        if !crate::prefs(cx).sandbox_writes || self.writes_anywhere {
            return None;
        }
        // Its tool servers run inside it: where they write is allowed too.
        let mut extra = self.writable.clone();
        extra.extend(keleuo_core::mcp::writes(&self.mcp_servers(cx)));
        Some(keleuo_core::sandbox::policy_for(&self.cwd, &self.backend.state_paths(), &extra))
    }

    /// The tool servers set up in Settings for this session's folder (none over SSH: they
    /// run where the agent does).
    fn mcp_servers(&self, cx: &App) -> Vec<keleuo_core::mcp::Server> {
        if self.host.is_remote() {
            return Vec::new();
        }
        let s = cx.global::<crate::AppSettings>().0.borrow();
        keleuo_core::mcp::servers_for(&s.mcp_servers, &s.folder_mcp, &self.cwd)
    }

    /// The tool servers file for the agent: the window's own (the messaging tools) and those
    /// set up in Settings; on this machine only.
    fn mcp_config(&self, cx: &App) -> Option<PathBuf> {
        if self.host.is_remote() {
            return None;
        }
        self.hooks.as_ref().and_then(|h| h.write_mcp(&self.mcp_servers(cx)))
    }

    /// A message from the session `from`: shown as its card, and sent to the agent as a turn
    /// (after the one it is in), marked as from that session, with how to answer.
    pub fn receive_message(&mut self, from: &str, text: &str, cx: &mut Context<Self>) {
        self.chat.apply(Event::AgentMessage { from: format!("From {from}"), text: text.to_string() });
        let framed = format!(
            "[A message from the Keleuo session \"{from}\", another agent (not the person):]\n\n{text}\n\n[If it needs an answer, use the keleuo send_message tool with to: \"{from}\".]"
        );
        self.quiet.push(framed.clone());
        self.send(&framed, cx);
        cx.notify();
    }

    /// A message the agent sent another session (`to`), as its card in this chat.
    pub fn note_outgoing(&mut self, id: u64, to: &str, text: &str, state: keleuo_core::conversation::SendState, cx: &mut Context<Self>) {
        self.chat.items.push(keleuo_core::conversation::Item::Outgoing { id, to: to.to_string(), text: text.to_string(), state });
        self.chat.revision += 1;
        cx.notify();
    }

    /// Where message `id` is now (allowed, denied, failed).
    pub fn set_outgoing(&mut self, id: u64, state: keleuo_core::conversation::SendState, cx: &mut Context<Self>) {
        for item in self.chat.items.iter_mut().rev() {
            if let keleuo_core::conversation::Item::Outgoing { id: i, state: s, .. } = item
                && *i == id
            {
                *s = state;
                break;
            }
        }
        self.chat.revision += 1;
        cx.notify();
    }

    /// A run of one of the person's tools the agent asked for, as its card (see `tool_runs`).
    pub fn note_tool_run(&mut self, id: u64, tool: &str, command: &str, state: keleuo_core::conversation::SendState, cx: &mut Context<Self>) {
        self.chat.items.push(keleuo_core::conversation::Item::ToolRun { id, tool: tool.to_string(), command: command.to_string(), state });
        self.chat.revision += 1;
        cx.notify();
    }

    /// Where tool run `id` is now (allowed, denied).
    pub fn set_tool_run(&mut self, id: u64, state: keleuo_core::conversation::SendState, cx: &mut Context<Self>) {
        for item in self.chat.items.iter_mut().rev() {
            if let keleuo_core::conversation::Item::ToolRun { id: i, state: s, .. } = item
                && *i == id
            {
                *s = state;
                break;
            }
        }
        self.chat.revision += 1;
        cx.notify();
    }

    /// Where the sign-in card is now (the last one).
    fn set_sign_in(&mut self, state: keleuo_core::conversation::SignIn, cx: &mut Context<Self>) {
        if let Some(keleuo_core::conversation::Item::SignIn(s)) =
            self.chat.items.iter_mut().rev().find(|i| matches!(i, keleuo_core::conversation::Item::SignIn(_)))
        {
            *s = state;
        }
        self.chat.revision += 1;
        cx.notify();
    }

    /// Start the agent's own sign-in: the command line to run in a terminal (the chat runs
    /// it), while Keleuo asks the agent every few seconds whether it is signed in yet.
    pub fn start_sign_in(&mut self, cx: &mut Context<Self>) -> Option<String> {
        use keleuo_core::conversation::{Item, SignIn};
        let spec = self.backend.sign_in()?;
        let line = std::iter::once(&spec.program).chain(&spec.args).map(|w| keleuo_core::host::shell_quote(w)).collect::<Vec<_>>().join(" ");
        // From the menu, before anything was refused: a card of its own to follow it on.
        let open = self.chat.items.iter().rev().find_map(|i| match i {
            Item::SignIn(s) => Some(*s != SignIn::Done),
            _ => None,
        });
        if open != Some(true) {
            self.chat.items.push(Item::SignIn(SignIn::Needed));
        }
        self.set_sign_in(SignIn::Waiting, cx);
        self.watch_sign_in(cx);
        Some(line)
    }

    /// Ask the agent whether it is signed in, now and then every few seconds for a quarter of an
    /// hour while the card waits; once it is, the card says so and the agent starts afresh
    /// with its next message (a running one keeps the sign-in it started with).
    pub fn watch_sign_in(&mut self, cx: &mut Context<Self>) {
        let (backend, host) = (self.backend.clone(), self.host.clone());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            for _ in 0..300 {
                let (backend, host) = (backend.clone(), host.clone());
                let signed = cx.background_spawn(async move { backend.signed_in(host.as_ref()) }).await;
                let waiting = this
                    .update(cx, |s, cx| {
                        if signed == Some(true) {
                            s.stop(cx);
                            s.set_sign_in(keleuo_core::conversation::SignIn::Done, cx);
                            return false;
                        }
                        s.chat.items.iter().rev().find_map(|i| match i {
                            keleuo_core::conversation::Item::SignIn(st) => Some(*st == keleuo_core::conversation::SignIn::Waiting),
                            _ => None,
                        }) == Some(true)
                    })
                    .unwrap_or(false);
                if !waiting {
                    return;
                }
                cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
            }
            let _ = this.update(cx, |s, cx| s.set_sign_in(keleuo_core::conversation::SignIn::Needed, cx));
        })
        .detach();
    }

    /// Ask the agent (its status command, off the window's thread) whether it is signed in;
    /// signed out, the chat shows the sign-in card at once.
    pub fn check_sign_in(&mut self, cx: &mut Context<Self>) {
        let (backend, host) = (self.backend.clone(), self.host.clone());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let signed = cx.background_spawn(async move { backend.signed_in(host.as_ref()) }).await;
            if signed == Some(false) {
                let _ = this.update(cx, |s, cx| {
                    s.chat.apply(keleuo_core::agent::Event::SignedOut);
                    s.chat.revision += 1;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Send the person's last message again (after signing in).
    pub fn send_again(&mut self, cx: &mut Context<Self>) {
        let last = self.chat.items.iter().rev().find_map(|i| match i {
            keleuo_core::conversation::Item::User(t) => Some(t.clone()),
            _ => None,
        });
        self.chat.refused_signed_out = false;
        if let Some(text) = last {
            self.send(&text, cx);
        }
    }

    /// Whether it runs on another machine.
    pub fn host_is_remote(&self) -> bool {
        self.host.is_remote()
    }

    /// The folder it works in.
    pub fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    /// Make it a reviewer: its folder not writable, the agent asking before it acts.
    pub fn make_reviewer(&mut self) {
        self.read_only = true;
        self.permission_mode = Some("default".into());
    }

    /// Whether this session's agent can be kept to its folder at all (Settings has it on, this
    /// machine has a sandbox, the agent runs here).
    pub fn can_confine(&self, cx: &App) -> bool {
        crate::prefs(cx).sandbox_writes
            && !self.host.is_remote()
            && keleuo_core::sandbox::system().support() == keleuo_core::sandbox::Support::Available
    }

    /// Let the agent write anywhere, or keep it to its folder again; it restarts to apply.
    pub fn set_writes_anywhere(&mut self, anywhere: bool, cx: &mut Context<Self>) {
        if self.writes_anywhere == anywhere {
            return;
        }
        self.writes_anywhere = anywhere;
        let said = if anywhere { "Writes allowed anywhere" } else { "Writes kept to this folder" };
        self.restart_with(said.into(), "with its writes changed".into(), cx);
    }

    /// Let the agent write in `folder` too (or, `allow` false, no longer); it restarts to apply.
    pub fn set_writable(&mut self, folder: PathBuf, allow: bool, cx: &mut Context<Self>) {
        let has = self.writable.contains(&folder);
        if has == allow {
            return;
        }
        if allow {
            self.writable.push(folder.clone());
        } else {
            self.writable.retain(|f| *f != folder);
        }
        let name = folder.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| folder.display().to_string());
        let said = if allow { format!("Writes allowed in {name}") } else { format!("Writes no longer allowed in {name}") };
        self.restart_with(said, "with its writes changed".into(), cx);
    }

    /// A write the sandbox refused: the card that says so, with ways to allow it.
    fn sandbox_issue(&self, path: Option<PathBuf>) -> keleuo_core::agent::Issue {
        use keleuo_core::agent::{Issue, IssueFix};
        let here = self.cwd.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let mut fixes = Vec::new();
        // The project the refused path is in (its repository, else its folder).
        let project = path.as_deref().and_then(|p| {
            let dir = p.ancestors().skip(1).find(|d| d.is_dir())?;
            Some(crate::shell::repo_of(dir).map(|(top, _)| top).unwrap_or_else(|| dir.to_path_buf()))
        });
        if let Some(project) = project {
            let name = project.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| project.display().to_string());
            fixes.push(IssueFix {
                id: format!("allow:{}", project.display()),
                label: format!("Allow writes in {name}"),
                preview: format!("The agent restarts and may also write inside {}, for this session.", project.display()),
            });
        }
        fixes.push(IssueFix {
            id: "anywhere".into(),
            label: "Allow writes anywhere".into(),
            preview: "The agent restarts with no limit on where it writes, for this session. The ⋯ menu over the chat keeps it to its folder again.".into(),
        });
        let what = path.map(|p| format!(" to {}", p.display())).unwrap_or_default();
        Issue {
            kind: SANDBOX_ISSUE.into(),
            title: "Kept out of a folder".into(),
            detail: format!(
                "This session's agent writes only inside {here} (and its own settings, caches and temporary folders). A write{what} was refused."
            ),
            fixes,
        }
    }

    fn start_terminal(&mut self, cx: &mut Context<Self>) {
        self.dormant = false;
        let started = std::time::Instant::now();
        if self.hooks.is_none() {
            self.hooks = hooks::register(cx.entity().downgrade(), self.backend.as_ref(), cx);
        }
        let fork_asked = std::mem::take(&mut self.fork_next);
        // Continue only a conversation that exists: a session started here that has had no
        // message yet has no file, and `--resume` would fail ("No conversation found"). Every
        // launch and relaunch (model, mode, view, take over, after an exit) comes through here.
        let has_messages = self
            .chat
            .session_id
            .as_deref()
            .is_some_and(|id| self.backend.session_has_messages(self.host.as_ref(), &self.cwd, id));
        let launch = Launch::decide(self.chat.session_id.is_some(), has_messages, fork_asked);
        let fork = launch == Launch::Fork;
        let (id, new) = match (launch, self.chat.session_id.clone()) {
            (Launch::New, Some(id)) => (id, true),
            (Launch::New, None) => (new_session_id(), true),
            (_, id) => (id.unwrap_or_else(new_session_id), false),
        };
        // On another machine the hooks' helper and settings are written there, by the same
        // connection, with a socket of this launch's own that it forwards to the window's.
        let remote = self.host.is_remote().then(|| {
            static LAUNCHES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = LAUNCHES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (hooks, home) = (self.hooks.as_ref()?, self.host.home_dir()?);
            let socket = PathBuf::from(format!("/tmp/keleuo-{}-{n}.sock", hooks.token()));
            hooks.remote(self.backend.as_ref(), &home.join(".cache/keleuo"), &socket).map(|r| (r, socket))
        }).flatten();
        let settings = match &remote {
            Some((r, _)) => Some(r.settings.clone()),
            None if self.host.is_remote() => None,
            None => self.hooks.as_ref().and_then(|h| h.settings.clone()),
        };
        let options = StartOptions {
            cwd: self.cwd.clone(),
            model: self.model.clone(),
            effort: self.effort.clone(),
            permission_mode: self.permission_mode.clone(),
            resume: None,
            fork,
            settings,
            sandbox: self.write_policy(cx),
            mcp_config: self.mcp_config(cx),
            env: keleuo_core::mcp::secret_env(&self.mcp_servers(cx)),
        };
        self.confined = options.sandbox.is_some();
        let Some(spec) = self.backend.terminal_program(&options, &id, new) else { return };
        if crate::perf::enabled() || keleuo_core::env_var("LOG_LAUNCH").is_ok() {
            eprintln!("keleuo: launch {} {}", spec.program, spec.args.iter().filter(|a| !a.ends_with(".json")).cloned().collect::<Vec<_>>().join(" "));
        }
        let command = match &remote {
            Some((r, socket)) => self.host.terminal_program_command(&spec, &r.files, Some((socket, &r.here))),
            None => self.host.terminal_program_command(&spec, &[], None),
        };
        let command = keleuo_core::sandbox::apply(command, options.sandbox.as_ref());
        let placeholder = Placeholder {
            lines: last_lines(&self.chat, 12),
            label: format!("Starting {}...", self.backend.name()),
        };
        let weak = cx.entity().downgrade();
        // A pasted image is kept here, where the agent on another machine cannot read it.
        // Its own clipboard key if it has one, else the kept copy's path typed in.
        let backend = self.backend.clone();
        let attach = self.backend.terminal_attach(std::path::Path::new("image.png"), false).is_some().then(|| {
            Rc::new(move |path: &std::path::Path, bracketed| backend.terminal_attach(path, bracketed)) as Rc<crate::terminal_view::AttachFn>
        });
        let paste = match self.backend.terminal_paste_image() {
            Some(key) => Some(PasteImage::Key(key)),
            None => attach.clone().map(PasteImage::Path),
        };
        let images = paste.filter(|_| !self.host.is_remote()).map(|paste| {
            let (w1, w2) = (weak.clone(), weak.clone());
            ImageSupport {
                paste,
                attach: attach.clone(),
                pasted: Rc::new(move |bytes, ext, cx| {
                    w1.update(cx, |s, _| {
                        let dir = keleuo_core::images::session_dir(s.chat.session_id.as_deref());
                        let path = dir.and_then(|d| keleuo_core::images::store(&d, &bytes, ext).ok())?;
                        s.pasted_images.push(path.clone());
                        Some(path)
                    })
                    .ok()
                    .flatten()
                }),
                image: Rc::new(move |n, cx| w2.upgrade().and_then(|s| s.read(cx).image_n(n))),
            }
        });
        let view = cx.new(|cx| {
            let mut v = TerminalView::new(command, cx);
            v.placeholder = Some(placeholder);
            v.images = images;
            v.agent = true;
            v
        });
        self.pasted_images.clear();
        // On each batch of output, shown or not (a hidden terminal draws nothing).
        let exit = cx.subscribe(&view, |s, view, ev: &TerminalEvent, cx| {
            if !matches!(ev, TerminalEvent::Output) {
                return;
            }
            // A parked interface (Chat showing) is not the session's to follow.
            if s.terminal.as_ref().is_none_or(|t| t.view != view) {
                return;
            }
            s.read_preview(&view, cx);
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
            } else if !s.ready {
                s.check_asking(&view, cx);
            }
        });
        // Up within a few seconds, or it is waiting on something in the terminal.
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(std::time::Duration::from_secs(ASK_AFTER_SECS)).await;
            let _ = this.update(cx, |s, cx| {
                if !s.ready && s.asking.is_none() && s.running(cx) {
                    s.asking = Some(Asking::Other);
                    s.unseen = true;
                    cx.notify();
                }
            });
        })
        .detach();
        self.ready = false;
        let links = cx.subscribe(&view, |_, _, ev: &TerminalEvent, cx| {
            if let TerminalEvent::OpenLink { line, column } = ev {
                cx.emit(SessionEvent::TerminalLink { line: line.clone(), column: *column });
            }
        });
        self.terminal = Some(TerminalRun { view, _exit: exit, _links: links });
        if fork {
            // The copy's id comes with the interface's start (`Hook::Started`).
            self.following = None;
        } else {
            self.chat.session_id = Some(id.clone());
            self.follow(&id, cx);
        }
        if crate::perf::enabled() {
            eprintln!("keleuo: agent interface started in {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
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
        self.restarting = None;
        for (text, images) in std::mem::take(&mut self.queued) {
            self.type_with_images(text, images, cx);
        }
    }

    /// Attach `images` (each pasted as a path, which the interface turns into `[Image #N]`),
    /// wait until it has taken them, then type `text`.
    fn type_with_images(&mut self, text: String, images: Vec<PathBuf>, cx: &mut Context<Self>) {
        if images.is_empty() {
            self.type_message(&text, cx);
            return;
        }
        if self.host.is_remote() {
            // The agent reads the files where it runs: copy them there first.
            let (host, local) = (self.host.clone(), images.clone());
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let there = cx.background_spawn(async move { copy_images_to(host.as_ref(), &local) }).await;
                let _ = this.update(cx, |s, cx| s.paste_images(text, there, images, cx));
            })
            .detach();
            return;
        }
        self.paste_images(text, images.clone(), images, cx);
    }

    /// Paste `paths` (the images where the agent runs), then type `text`; `shown` are the same
    /// images here, for Chat.
    fn paste_images(&mut self, text: String, paths: Vec<PathBuf>, shown: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(view) = self.terminal_view() else { return };
        let Some(pty) = view.read(cx).pty.clone() else { return };
        let bracketed = view.read(cx).bracketed_paste();
        let pieces: Vec<Vec<u8>> = paths.iter().filter_map(|p| self.backend.terminal_attach(p, bracketed)).collect();
        self.pasted_images.extend(shown);
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            for piece in pieces {
                pty.write(piece);
                // It reads the file ("Pasting..."): wait for that before the next.
                for _ in 0..40 {
                    cx.background_executor().timer(std::time::Duration::from_millis(75)).await;
                    if !view.update(cx, |v, _| v.screen_text().contains("Pasting")) {
                        break;
                    }
                }
            }
            let _ = this.update(cx, |s, cx| {
                if text.is_empty() {
                    if let Some(pty) = s.terminal_view().and_then(|v| v.read(cx).pty.clone()) {
                        pty.write(b"\r".to_vec());
                    }
                } else {
                    // A space after the image markers, then the text.
                    let text = format!(" {text}");
                    s.type_message(&text, cx);
                }
            });
        })
        .detach();
    }

    /// The image behind the interface's `[Image #N]`: the Nth pasted into this run, else the
    /// Nth in the conversation.
    pub fn image_n(&self, n: usize) -> Option<PathBuf> {
        let i = n.checked_sub(1)?;
        self.pasted_images.get(i).cloned().or_else(|| {
            self.chat
                .items
                .iter()
                .filter_map(|it| match it {
                    keleuo_core::conversation::Item::Images(p) => Some(p),
                    _ => None,
                })
                .flatten()
                .nth(i)
                .cloned()
        })
    }

    /// Switch the model. The interface restarts on the same conversation with it (between
    /// turns), rather than typing its `/model` command: Claude Code's `/model` also saves the
    /// choice as the person's default for every new session, in their own settings.
    /// Ask the agent for its models, once, off the UI thread (the model menu then lists them).
    pub fn want_models(&mut self, cx: &mut Context<Self>) {
        if self.listed_models.is_some() {
            return;
        }
        self.listed_models = Some(Vec::new());
        let (backend, host) = (self.backend.clone(), self.host.clone());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let models = cx.background_spawn(async move { backend.list_models(host.as_ref()) }).await;
            let _ = this.update(cx, |s, cx| {
                s.listed_models = Some(models);
                cx.notify();
            });
        })
        .detach();
    }

    pub fn set_model(&mut self, model: &str, cx: &mut Context<Self>) {
        self.model = Some(model.to_string());
        self.restart_with(format!("Model: {model}"), format!("with {model}"), cx);
    }

    /// Switch the effort level (`None`: the agent's default), as the model is switched.
    pub fn set_effort(&mut self, effort: Option<String>, cx: &mut Context<Self>) {
        if self.effort == effort {
            return;
        }
        let name = effort.clone().unwrap_or_else(|| "default".into());
        self.effort = effort;
        self.restart_with(format!("Effort: {name}"), format!("at {name} effort"), cx);
    }

    /// Start the agent again with what changed: now if it is idle, else when the turn ends, or
    /// with the next message when it is not running. `said` goes in the chat.
    fn restart_with(&mut self, said: String, restarting: String, cx: &mut Context<Self>) {
        if self.interactive() && self.running(cx) {
            if self.chat.busy {
                self.restart_after_turn = true;
                self.chat.push_notice(format!("{said}. Applies when this turn ends."));
            } else {
                self.stop(cx);
                self.restarting = Some(restarting);
                self.start_terminal(cx);
                self.chat.push_notice(format!("{said}."));
            }
        } else {
            self.stop(cx);
            self.chat.push_notice(format!("{said}. Applies from the next message."));
        }
        cx.notify();
    }

    /// Keep the session's mode for when it opens again.
    fn remember_mode(&self, mode: Option<&str>, cx: &mut Context<Self>) {
        if let (Some(id), Some(mode)) = (&self.chat.session_id, mode) {
            let settings = cx.global::<crate::AppSettings>().0.clone();
            settings.borrow_mut().set_mode(id, mode);
            let _ = settings.borrow().save();
        }
    }

    /// Switch the permission mode. With the interface running, press its mode key until its
    /// status line shows the mode (as a person would); otherwise restart with it.
    pub fn set_mode(&mut self, mode: Option<String>, cx: &mut Context<Self>) {
        let mode = Some(mode.unwrap_or_else(|| "default".into()));
        self.remember_mode(mode.as_deref(), cx);
        let cycle = self.backend.mode_cycle();
        let (Some((modes, key)), true, Some(view)) = (cycle, self.interactive() && self.ready, self.terminal_view()) else {
            self.set_permission_mode(mode, cx);
            return;
        };
        let target = mode.clone().unwrap_or_else(|| "default".into());
        let labels: Vec<String> = modes.iter().map(|(_, l)| l.clone()).filter(|l| !l.is_empty()).collect();
        let want = modes.iter().find(|(m, _)| *m == target).map(|(_, l)| l.clone()).unwrap_or_default();
        self.permission_mode = mode;
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            for _ in 0..(modes.len() + 2) {
                let (pty, done) = view.update(cx, |v, _| {
                    let screen = v.screen_text();
                    // The interface's last lines with text (a short conversation leaves the
                    // screen's last rows blank, and its status line higher up).
                    let footer: String = screen.lines().rev().filter(|l| !l.trim().is_empty()).take(4).collect::<Vec<_>>().join("\n");
                    let shown = if want.is_empty() { !labels.iter().any(|l| footer.contains(l.as_str())) } else { footer.contains(want.as_str()) };
                    (v.pty.clone(), shown)
                });
                if done {
                    let _ = this.update(cx, |s, cx| {
                        s.live_mode = Some(target.clone());
                        cx.notify();
                    });
                    return;
                }
                let Some(pty) = pty else { return };
                pty.write(key.clone());
                cx.background_executor().timer(std::time::Duration::from_millis(200)).await;
            }
            let _ = this.update(cx, |s, cx| {
                s.chat.push_notice("Could not switch the mode from here; use Shift+Tab in the terminal.");
                cx.notify();
            });
        })
        .detach();
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
        let seen = self.prompts_seen;
        let (view, text) = (run.view.clone(), text.to_string());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            for piece in rest {
                // A separate read for each piece, so the paste ends before Enter arrives.
                cx.background_executor().timer(std::time::Duration::from_millis(30)).await;
                pty.write(piece);
            }
            // The interface may drop the Enter while it is still busy (starting on a long
            // conversation, say): no word that the message went, and the message still in its
            // input box, means press Enter again.
            for _ in 0..SUBMIT_RETRIES {
                cx.background_executor().timer(SUBMIT_CHECK).await;
                let again = this.update(cx, |s, cx| {
                    s.prompts_seen == seen
                        && view.read(cx).recent_lines(80).is_some_and(|(lines, _)| s.backend.message_unsent(&lines, &text))
                });
                match again {
                    Ok(true) => {
                        pty.write(b"\r".to_vec());
                        // Said, so how often the interface loses an Enter (and when) shows.
                        let _ = this.update(cx, |s, cx| {
                            s.chat.push_notice("The message hadn't gone; pressed Enter again.");
                            cx.notify();
                        });
                    }
                    _ => break,
                }
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
        // It is past any question it was asking.
        self.asking = None;
        if let Some(mode) = self.backend.hook_mode(&call.payload) {
            // Switched in the terminal (Shift+Tab): the session's mode from now on, in Chat too.
            if self.permission_mode.as_deref() != Some(mode.as_str()) {
                self.remember_mode(Some(&mode), cx);
                self.permission_mode = Some(mode.clone());
            }
            self.live_mode = Some(mode);
        }
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
                self.end_parked_agent();
                self.prompts_seen += 1;
                // The agent took a message: the one sent from here, if it is waiting.
                self.chat.mark_seen();
                let t = plain(&text);
                // Empty: a prompt the interface made itself (a report, a task notice), which
                // may not start a turn; the work it starts marks the session busy.
                if !t.is_empty() {
                    if !self.echo.iter().any(|e| *e == t) {
                        // Typed in the terminal: show it now; the file's copy is skipped.
                        self.chat.push_user(&t);
                        self.push_echo(t);
                    }
                    self.chat.busy = true;
                    self.chat.status = Some("working".into());
                }
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
                    self.summarize_turn(cx);
                }
                if std::mem::take(&mut self.restart_after_turn) {
                    self.stop(cx);
                    self.restarting = self.model.as_ref().map(|m| format!("with {m}"));
                    self.start_terminal(cx);
                }
            }
            Hook::Attention(_) => self.unseen = true,
        }
        self.send_waiting(cx);
        self.chat.revision += 1;
        cx.notify();
    }

    fn push_echo(&mut self, text: String) {
        self.echo.push_back(plain(&text));
        if self.echo.len() > 8 {
            self.echo.pop_front();
        }
    }

    // ----- the structured agent (backends without a terminal interface) -----

    fn start(&mut self, cx: &mut Context<Self>) -> anyhow::Result<()> {
        // Registered for its tool server (the messaging tools), as the terminal view is for
        // its hooks too.
        if self.hooks.is_none() && !self.host.is_remote() {
            self.hooks = hooks::register(cx.entity().downgrade(), self.backend.as_ref(), cx);
        }
        let options = StartOptions {
            cwd: self.cwd.clone(),
            model: self.model.clone(),
            effort: self.effort.clone(),
            permission_mode: self.permission_mode.clone(),
            resume: self.chat.session_id.clone(),
            fork: std::mem::take(&mut self.fork_next),
            settings: None,
            sandbox: self.write_policy(cx),
            mcp_config: self.mcp_config(cx),
            env: keleuo_core::mcp::secret_env(&self.mcp_servers(cx)),
        };
        self.confined = options.sandbox.is_some();
        let (tx, rx) = async_channel::unbounded::<Message>();
        self.agent.start(
            self.host.clone(),
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
                    session.send_waiting(cx);
                    session.unseen = true;
                    session.follow_workflows(cx);
                    cx.notify();
                });
                if ok.is_err() {
                    break;
                }
                // What arrives in the next tenth of a second is taken as one batch: a reply can
                // stream in dozens of pieces a second, and each change of the session draws the
                // window (the chat, its row, the working line) again. The first piece after a
                // quiet spell still shows at once.
                cx.background_executor().timer(BATCH_EVERY).await;
            }
        })
    }

    /// While a workflow runs (Claude Code's Workflow tool, shown as a background agent): every
    /// few seconds its journal is read, off the UI thread, and its card says how far it is
    /// ("9 of 13 agents done"). Claude Code reports a workflow's agents nowhere else Keleuo reads.
    fn follow_workflows(&mut self, cx: &mut Context<Self>) {
        if self.workflow_poll.is_some() || self.host.is_remote() || self.running_workflows().is_empty() {
            return;
        }
        self.workflow_poll = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let mut shown: HashMap<String, (usize, usize)> = HashMap::new();
            loop {
                let Ok(running) = this.update(cx, |s, _| s.running_workflows()) else { break };
                if running.is_empty() {
                    let _ = this.update(cx, |s, _| s.workflow_poll = None);
                    break;
                }
                let read = cx
                    .background_spawn(async move {
                        running
                            .into_iter()
                            .map(|(id, dir)| {
                                let journal = std::fs::read_to_string(dir.join("journal.jsonl")).unwrap_or_default();
                                (id, keleuo_core::workflow::progress(&journal))
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;
                let changed: Vec<_> = read.into_iter().filter(|(id, p)| shown.get(id) != Some(p)).collect();
                if !changed.is_empty() {
                    for (id, p) in &changed {
                        shown.insert(id.clone(), *p);
                    }
                    let _ = this.update(cx, |s, cx| {
                        for (id, (done, started)) in changed {
                            let what = if started == 0 { "starting".to_string() } else { format!("{done} of {started} agents done") };
                            s.chat.apply(Event::SubagentProgress { id, current_tool: Some(what), tokens: None, tool_uses: None, duration_ms: None });
                        }
                        cx.notify();
                    });
                }
                cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
            }
        }));
    }

    /// The workflows running, with where each keeps its journal.
    fn running_workflows(&self) -> Vec<(String, PathBuf)> {
        self.chat
            .running_agents()
            .filter(|a| a.kind == "workflow")
            .filter_map(|a| {
                let output = self.chat.items.iter().find_map(|i| match i {
                    keleuo_core::conversation::Item::Tool(t) if t.id == a.tool_use_id => t.result.clone(),
                    _ => None,
                })?;
                Some((a.tool_use_id.clone(), keleuo_core::workflow::dir(&output)?))
            })
            .collect()
    }

    fn handle(&mut self, message: Message, cx: &mut Context<Self>) {
        match message {
            Message::Event(event) => {
                if let Event::UserText(text) = &event
                    && let Some(i) = self.echo.iter().position(|e| *e == plain(text))
                {
                    // Shown already, when it was sent; the agent has it now.
                    self.echo.remove(i);
                    self.chat.mark_seen();
                    return;
                }
                if let Event::UserImages(_) = &event
                    && self.skip_images > 0
                {
                    self.skip_images -= 1;
                    return;
                }
                if matches!(event, Event::Text { .. } | Event::TextDelta(_) | Event::ToolCall { .. })
                    && let Some(at) = self.sent_at.take()
                {
                    eprintln!("keleuo: reply shows {:.0} ms after Send", at.elapsed().as_secs_f64() * 1000.0);
                }
                // A write the sandbox refused: said on the tool's card, with ways to allow it.
                let refused = match &event {
                    Event::ToolResult { id, output, .. } if self.confined => keleuo_core::sandbox::refused(output).map(|p| (id.clone(), p)),
                    _ => None,
                };
                let applied = self.chat.apply(event);
                if let Some((id, path)) = refused {
                    let issue = self.sandbox_issue(path);
                    self.chat.apply(Event::ToolIssue { id, issue });
                }
                if let Some(path) = applied.file_changed {
                    cx.emit(SessionEvent::FileChanged(path));
                }
                self.watch_turn(cx);
                if applied.turn_ended {
                    self.find_command_changes(cx);
                    cx.emit(SessionEvent::TurnEnded);
                    self.summarize_turn(cx);
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

    /// When a turn has just begun, read the git working tree (in the background) to find later
    /// what commands changed in it (here or over SSH). Not for turns read back from a file.
    fn watch_turn(&mut self, cx: &mut Context<Self>) {
        let turn = self.chat.turn_start();
        if !self.chat.busy
            || !crate::prefs(cx).command_changes
            || !matches!(self.chat.items.get(turn), Some(keleuo_core::conversation::Item::User(_)))
            || self.git_turn.as_ref().is_some_and(|(t, _)| *t == turn)
        {
            return;
        }
        self.git_turn = Some((turn, None));
        let (host, cwd) = (self.host.clone(), self.cwd.clone());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let base =
                cx.background_executor().spawn(async move { keleuo_core::command_changes::baseline(host.as_ref(), &cwd) }).await;
            let _ = this.update(cx, |s, _| {
                if let Some((t, b)) = &mut s.git_turn
                    && *t == turn
                {
                    *b = base;
                }
            });
        })
        .detach();
    }

    /// The turn ended: add what commands changed in it to its card.
    fn find_command_changes(&mut self, cx: &mut Context<Self>) {
        let turn = self.chat.turn_start();
        let Some((_, Some(base))) = self.git_turn.take_if(|(t, b)| *t == turn && b.is_some()) else { return };
        let (host, skip) = (self.host.clone(), self.chat.edited_in_turn(turn));
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let found =
                cx.background_executor().spawn(async move { keleuo_core::command_changes::since(host.as_ref(), &base, &skip) }).await;
            let _ = this.update(cx, |s, cx| {
                s.chat.add_command_changes(turn, found);
                cx.notify();
            });
        })
        .detach();
    }

    /// Ask for the summary of the turn that just ended, when summaries are set up (see
    /// [`keleuo_core::summary`]). A moment later, so the files commands changed are found and
    /// a followed session file has caught up; off the UI thread; kept for when the session is
    /// opened again.
    fn summarize_turn(&mut self, cx: &mut Context<Self>) {
        let config = cx.global::<crate::AppSettings>().0.borrow().summaries.clone();
        if !config.ready() {
            return;
        }
        let from = self.chat.turn_start();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
            let asked = this.update(cx, |s, cx| {
                let turn = summary::TurnText::of(&s.chat, from, &config)?;
                let key = turn.key();
                if !s.summarized.insert(key.clone()) {
                    return None;
                }
                s.chat.set_summary(&key, summary::State::Pending);
                cx.notify();
                Some((turn, key, s.chat.session_id.clone()))
            });
            let Ok(Some((turn, key, session))) = asked else { return };
            let result = cx.background_executor().spawn(async move { summary::summarize(&config, &turn) }).await;
            let _ = this.update(cx, |s, cx| {
                let state = match result {
                    Ok(done) => {
                        if let Some(id) = &session {
                            summary::keep(id, &key, &done);
                        }
                        summary::State::Ready(done)
                    }
                    Err(e) => {
                        eprintln!("keleuo: turn summary: {e:#}");
                        summary::State::Failed(keleuo_core::conversation::one_line(&format!("{e:#}"), 160))
                    }
                };
                s.chat.set_summary(&key, state);
                cx.notify();
            });
        })
        .detach();
    }

    /// Show the kept summaries of this session's turns.
    pub fn attach_summaries(&mut self) {
        if let Some(id) = self.chat.session_id.clone() {
            self.chat.attach_summaries(&summary::cached(&id));
        }
    }

    // ----- what the window asks -----

    /// Send a user turn, starting the agent when needed.
    /// Where the session's folder is (this machine, or over SSH).
    pub fn host(&self) -> Arc<dyn Host> {
        self.host.clone()
    }

    pub fn send(&mut self, text: &str, cx: &mut Context<Self>) {
        self.send_with(text, Vec::new(), cx);
    }

    /// Send a user turn with images (files). Pastes marked in `text` (see
    /// [`keleuo_core::paste`]) show folded in the chat and go without their markers. While the
    /// agent works it waits its turn (see [`keleuo_core::outbox`]), as it does behind others
    /// waiting; it shows in the chat when it goes.
    pub fn send_with(&mut self, text: &str, images: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.outbox_paused = false;
        if (self.chat.busy || !self.outbox.is_empty())
            && !(keleuo_core::paste::without_markers(text).trim().is_empty() && images.is_empty())
        {
            self.outbox.push(text, images);
            self.send_waiting(cx);
            cx.notify();
            return;
        }
        self.deliver(text, images, cx);
    }

    /// Send a user turn now, starting the agent when needed.
    fn deliver(&mut self, text: &str, images: Vec<PathBuf>, cx: &mut Context<Self>) {
        if self.closing {
            self.held.push((text.to_string(), images));
            cx.notify();
            return;
        }
        let shown = text.trim();
        let sent = keleuo_core::paste::without_markers(shown);
        let text = sent.trim();
        if text.is_empty() && images.is_empty() {
            return;
        }
        // A message from another session shows as its card (already there), not as the
        // person's message.
        let quiet = match self.quiet.iter().position(|q| q.trim() == shown) {
            Some(i) => {
                self.quiet.remove(i);
                true
            }
            None => false,
        };
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
            self.end_parked_agent();
            if !text.is_empty() {
                if !quiet {
                    self.chat.push_user(shown);
                }
                self.push_echo(text.to_string());
            }
            if !images.is_empty() {
                self.chat.apply(Event::UserImages(images.clone()));
                self.skip_images += 1;
            }
            // An agent with no hooks cannot say its turn began: it has, with this message (its
            // end comes with the session's record).
            if self.backend.hook_settings(&|_| String::new()).is_none() && !(text.is_empty() && images.is_empty()) {
                self.chat.busy = true;
                self.chat.status = Some("working".into());
            }
            if !self.running(cx) {
                self.start_terminal(cx);
            }
            if self.ready {
                self.type_with_images(text.to_string(), images, cx);
            } else {
                self.queued.push((text.to_string(), images));
            }
            cx.notify();
            return;
        }
        // The idle interface ends before this agent writes to the conversation.
        if let Some(p) = self.parked.take() {
            self.held.push((shown.to_string(), images));
            self.end_interface(p.run.view.clone(), p.pty, cx);
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
        if !text.is_empty() {
            if quiet {
                self.chat.begin_turn();
            } else {
                self.chat.push_user(shown);
            }
        } else if !images.is_empty() {
            // Images alone start a turn too: what waits goes after it.
            self.chat.busy = true;
            self.chat.status = Some("sending".into());
        }
        let images = if self.agent.takes_images() {
            images
        } else {
            if !images.is_empty() {
                self.chat.push_notice(format!("{} cannot take images here; sent the text only.", self.backend.name()));
            }
            Vec::new()
        };
        if !images.is_empty() {
            self.chat.apply(Event::UserImages(images.clone()));
        }
        self.watch_turn(cx);
        let sent = if images.is_empty() { self.agent.send(text) } else { self.agent.send_with_images(text, &images) };
        if let Err(e) = sent {
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

    /// Answer a question the agent asked (its request to run the asking tool): allowed, with
    /// the chosen labels for each question added to the tool's input.
    pub fn answer_question(&mut self, request_id: &str, answers: Vec<(String, Vec<String>)>, cx: &mut Context<Self>) {
        let shown = answers.iter().map(|(_, a)| a.join(", ")).collect::<Vec<_>>().join("; ");
        let Some(input) = self.chat.answer_question(request_id, shown) else { return };
        let input = keleuo_core::questions::answered(&input, &answers);
        if let Some(reply) = self.waiting.remove(request_id) {
            reply.send(&self.backend.hook_reply_with(&input));
        } else {
            let _ = self.agent.answer_permission(request_id, true, &input);
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

    /// The person stops the turn: what waits stays until they send it, or send another.
    pub fn interrupt(&mut self, cx: &mut Context<Self>) {
        if self.chat.busy && !self.outbox.is_empty() {
            self.outbox_paused = true;
        }
        self.stop_turn(cx);
    }

    /// Stop the current turn, keeping the agent.
    fn stop_turn(&mut self, cx: &mut Context<Self>) {
        if self.interactive() {
            if let Some(pty) = self.terminal.as_ref().and_then(|t| t.view.read(cx).pty.clone()) {
                pty.write(self.backend.terminal_interrupt());
            }
            // No hooks to say the turn stopped (nor a record of a stopped turn): it has.
            if self.backend.hook_settings(&|_| String::new()).is_none() && self.chat.busy {
                self.chat.apply(Event::Interrupted);
                self.send_waiting(cx);
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
        // What the interface last said no longer holds.
        self.live_mode = None;
        let now = self.interactive() && self.running(cx) && !self.chat.busy;
        self.stop(cx);
        if now {
            self.restarting = Some("in the new permission mode".into());
            self.start_terminal(cx);
        }
        self.chat.push_notice(format!(
            "Permission mode: {}. {}",
            mode_label(self.backend.as_ref(), self.permission_mode.as_deref()),
            if now { "Restarted with it." } else { "Applies from the next message." }
        ));
        cx.notify();
    }

    // ----- views -----

    pub fn terminal_view(&self) -> Option<Entity<TerminalView>> {
        self.terminal.as_ref().map(|t| t.view.clone())
    }

    /// Show Chat or Terminal. With the hybrid setting nothing is started or stopped (both are
    /// views of one process); without it the agent restarts on the conversation in the other
    /// view, between turns only.
    pub fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        if view == self.view || (view == View::Terminal && !self.backend.capabilities().terminal_view) || self.switch_blocked().is_some() {
            return;
        }
        crate::perf::switch_begin(if view == View::Terminal { "to Terminal" } else { "to Chat" });
        if self.hybrid {
            if view == View::Terminal && self.terminal.is_none() && self.waiting_for.is_none() {
                self.start_terminal(cx);
            }
            self.view = view;
        } else if view == View::Terminal {
            // Chat's agent is idle: it waits, in case Chat shows again before anything changes.
            self.agent_parked_at = if self.agent.running() { self.session_len() } else { None };
            if self.agent_parked_at.is_none() {
                self.end_agent();
            }
            self.view = view;
            match self.parked.take() {
                // Nothing new since the interface was hidden: it shows as it was.
                Some(p) if p.len.is_some_and(|l| !self.moved_on(l)) && !p.run.view.read(cx).exited => {
                    self.terminal = Some(p.run);
                    self.ready = p.ready;
                    if let Some(id) = self.chat.session_id.clone() {
                        self.follow(&id, cx);
                    }
                }
                stale => {
                    if let Some(pty) = stale.and_then(|p| p.pty) {
                        crate::pty::end_for_certain(&pty);
                    }
                    if self.waiting_for.is_none() {
                        self.start_terminal(cx);
                    }
                }
            }
            self.park_timer(cx);
        } else {
            self.view = view;
            // The interface waits, idle, in case Terminal shows again before anything changes.
            if let Some(run) = self.terminal.take()
                && !run.view.read(cx).exited
            {
                let pty = run.view.read(cx).pty.clone();
                self.parked = Some(Parked { len: self.session_len(), ready: self.ready, run, pty });
            }
            self.following = None;
            self.ready = false;
            // Chat's own agent, if it waited: kept when nothing changed meanwhile.
            if let Some(len) = self.agent_parked_at.take()
                && !(self.agent.running() && !self.moved_on(len))
            {
                self.end_agent();
            }
            self.park_timer(cx);
        }
        self.remember_view(cx);
        cx.notify();
    }

    /// The session file's length now.
    fn session_len(&self) -> Option<u64> {
        let id = self.chat.session_id.as_deref()?;
        self.backend.session_len(self.host.as_ref(), &self.cwd, id)
    }

    /// Whether the conversation has a turn past `len` of its file (an idle agent from then is
    /// out of date).
    fn moved_on(&self, len: u64) -> bool {
        self.chat.session_id.as_deref().is_none_or(|id| self.backend.session_moved_on(self.host.as_ref(), &self.cwd, id, len))
    }

    /// End Chat's headless agent.
    fn end_agent(&mut self) {
        if self.agent.running() {
            self.agent.stop();
            self._reader = None;
            self.chat.process_ended();
        }
    }

    /// The interface works now: Chat's agent, waiting while Terminal shows, would fall behind.
    fn end_parked_agent(&mut self) {
        if !self.hybrid && self.agent_parked_at.take().is_some() {
            self.end_agent();
        }
    }

    /// A parked agent ends when its view has not shown again for a while.
    fn park_timer(&mut self, cx: &mut Context<Self>) {
        self.switches += 1;
        let at = self.switches;
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(PARK_FOR).await;
            let _ = this.update(cx, |s, cx| {
                if s.switches != at {
                    return;
                }
                if let Some(pty) = s.parked.take().and_then(|p| p.pty) {
                    crate::pty::end_for_certain(&pty);
                }
                if s.view == View::Terminal && s.agent_parked_at.take().is_some() {
                    s.end_agent();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// End the interface in `view` before Chat's agent works: a message sent meanwhile waits
    /// for it to have exited (two processes on one conversation would each write their own
    /// turns to its file), then goes.
    fn end_interface(&mut self, view: Entity<TerminalView>, pty: Option<Rc<crate::pty::Pty>>, cx: &mut Context<Self>) {
        let Some(pty) = pty.filter(|_| !view.read(cx).exited) else {
            for (text, images) in std::mem::take(&mut self.held) {
                self.send_with(&text, images, cx);
            }
            return;
        };
        let group = pty.end();
        self.closing = true;
        self.restarting = Some("in Chat".into());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            // Ended when its process group is gone: killed if the hang-up has not done it in
            // two seconds, given a moment more to go, and then the message goes regardless.
            if let Some(group) = group {
                for tick in 0..30 {
                    if crate::pty::gone(group) {
                        break;
                    }
                    if tick == 20 {
                        crate::pty::kill(group);
                    }
                    cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
                }
            }
            let _ = this.update(cx, |s, cx| {
                s.closing = false;
                s.restarting = None;
                for (text, images) in std::mem::take(&mut s.held) {
                    s.send_with(&text, images, cx);
                }
                s.send_waiting(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn remember_view(&self, cx: &mut Context<Self>) {
        if let Some(id) = &self.chat.session_id {
            let settings = cx.global::<crate::AppSettings>().0.clone();
            settings.borrow_mut().set_view(id, self.view.name());
            let _ = settings.borrow().save();
        }
    }

    /// End the session's agent: its process (the terminal interface is hung up on, not just
    /// let go: other handles may keep its view alive) and the following of its file.
    pub fn stop(&mut self, cx: &mut App) {
        if let Some(run) = self.terminal.take()
            && let Some(pty) = run.view.read(cx).pty.clone()
        {
            crate::pty::end_for_certain(&pty);
        }
        self.release();
    }

    /// Let go of the agent and the file following (all a drop can do, with no app at hand).
    fn release(&mut self) {
        if let Some(pty) = self.parked.take().and_then(|p| p.pty) {
            crate::pty::end_for_certain(&pty);
        }
        self.agent_parked_at = None;
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

/// How long after sending a message its submission is checked, and how many times Enter is
/// pressed again if it still sits unsent.
const SUBMIT_CHECK: std::time::Duration = std::time::Duration::from_millis(1200);
const SUBMIT_RETRIES: usize = 3;

/// How often at most the reply preview is read off the terminal.
const PREVIEW_EVERY: std::time::Duration = std::time::Duration::from_millis(100);
/// Lines of scrollback and screen the preview is looked for in (a reply taller than this gets
/// no preview: its start is out of reach).
const PREVIEW_LINES: usize = 400;

/// What a permission mode is called, among `backend`'s modes; one it does not have (another
/// agent's) counts as its first, its default.
pub fn mode_label(backend: &dyn Backend, mode: Option<&str>) -> &'static str {
    let modes = backend.permission_modes();
    match mode {
        Some("bypassPermissions") if !modes.iter().any(|(m, _)| *m == "bypassPermissions") => "Bypass permissions",
        _ => modes
            .iter()
            .find(|(name, _)| Some(*name) == mode)
            .or(modes.first())
            .map_or("Default", |(_, label)| *label),
    }
}

/// A message as compared with its copy in the session file: without the interface's image
/// markers (`[Image #1]`), its paste markers, and outer spaces.
fn plain(text: &str) -> String {
    let text = keleuo_core::paste::without_markers(text);
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(i) = rest.find("[Image #") {
        out.push_str(&rest[..i]);
        match rest[i..].find(']') {
            Some(j) => rest = &rest[i + j + 1..],
            None => {
                rest = &rest[i..];
                break;
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The conversation's last `n` lines of text, oldest first, `true` for the person's.
fn last_lines(chat: &Conversation, n: usize) -> Vec<(bool, String)> {
    use keleuo_core::conversation::Item;
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
        self.release();
    }
}

/// Copy `images` (here) into a folder of Keleuo's on `host`, giving their paths there; one that
/// cannot be copied is left out.
fn copy_images_to(host: &dyn keleuo_core::host::Host, images: &[PathBuf]) -> Vec<PathBuf> {
    let Some(dir) = host.home_dir().map(|h| h.join(".cache/keleuo/images")) else { return Vec::new() };
    if host.create_dir_all(&dir).is_err() {
        return Vec::new();
    }
    images
        .iter()
        .filter_map(|p| {
            let there = dir.join(p.file_name()?);
            host.write(&there, &std::fs::read(p).ok()?).ok()?;
            Some(there)
        })
        .collect()
}

#[cfg(test)]
mod fill_tests {
    use super::Fill;

    /// Measured against where the agent compacts, not the whole window.
    #[test]
    fn the_context_turns_amber_then_red_on_the_way_to_compaction() {
        let at = 167_000; // a 200k window, compacting 33k short of it
        assert_eq!(Fill::of(100_000, at), Fill::Room);
        assert_eq!(Fill::of(117_000, at), Fill::Filling);
        assert_eq!(Fill::of(151_000, at), Fill::Nearly);
        assert_eq!(Fill::of(190_000, at), Fill::Nearly);
        assert_eq!(Fill::of(5, 0), Fill::Nearly);
    }
}
