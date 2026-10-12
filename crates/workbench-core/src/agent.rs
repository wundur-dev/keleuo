//! The agent behind a session, in neutral terms. The window renders [`Event`]s and talks to an
//! [`Agent`] (start, send, answer a permission prompt, interrupt, stop) made by a [`Backend`],
//! whose [`Capabilities`] say which optional features it has (skills, earlier sessions,
//! subagents, permission prompts, models). Backends are found in a [`Registry`].
//!
//! A backend maps its own wire format onto these events inside its own module (Claude Code's
//! `stream-json` in `backends::claude`); nothing outside a backend knows its format.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use crate::conversation::Conversation;
use crate::diff::FileDiff;
use crate::host::Host;
use crate::skills::Catalog;

/// One thing that happened in a conversation.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The agent process is up.
    Started { session_id: Option<String>, model: Option<String>, permission_mode: Option<String>, cwd: Option<String> },
    /// A short progress note ("thinking", "requesting").
    Status(String),
    /// A new block of the agent's text begins (streamed in [`Event::TextDelta`]s).
    TextStart,
    /// More of the text being streamed.
    TextDelta(String),
    /// A complete block of text (replaces the streamed one). `parent`: a subagent's.
    Text { text: String, parent: Option<String> },
    /// The person's message (from a history being replayed).
    UserText(String),
    /// A tool call, or more of its details. `diff`: what it will change, when known.
    ToolCall { id: String, name: String, summary: String, input: Value, diff: Option<FileDiff>, parent: Option<String> },
    /// A tool's result. `diff` and `file_changed`: what it changed.
    ToolResult { id: String, output: String, is_error: bool, diff: Option<FileDiff>, file_changed: Option<PathBuf>, parent: Option<String> },
    /// The agent asks before doing something.
    Permission { request_id: String, tool: String, summary: String, description: Option<String>, input: Value },
    /// A subagent began (`id` is the tool call that started it).
    SubagentStarted { id: String, description: String, kind: String },
    /// A subagent's progress.
    SubagentProgress { id: String, current_tool: Option<String>, tokens: Option<u64>, tool_uses: Option<u64>, duration_ms: Option<u64> },
    /// A subagent finished.
    SubagentEnded { id: String, failed: bool, summary: Option<String>, tokens: Option<u64>, duration_ms: Option<u64> },
    /// A turn ended, with what it used. `cost_total_usd` is the process total so far.
    TurnEnded {
        error: Option<String>,
        cost_total_usd: Option<f64>,
        input_tokens: u64,
        output_tokens: u64,
        duration_ms: Option<u64>,
        session_id: Option<String>,
    },
    /// A quiet note (warnings the agent printed).
    Notice(String),
}

/// What an agent's reader threads report.
#[derive(Clone, Debug)]
pub enum Message {
    Event(Event),
    /// A line the agent wrote to its error output.
    Log(String),
    /// The agent process ended.
    Closed,
}

/// Where every message from the agent goes (called on a reader thread).
pub type Sink = Arc<dyn Fn(Message) + Send + Sync>;

/// What a session asks of its agent.
#[derive(Clone, Debug, Default)]
pub struct StartOptions {
    pub cwd: PathBuf,
    pub model: Option<String>,
    /// `None`: the backend's own default (ask before acting).
    pub permission_mode: Option<String>,
    /// Continue this earlier conversation.
    pub resume: Option<String>,
    /// Continue it in a copy (the original keeps running elsewhere).
    pub fork: bool,
}

/// A running conversation with an agent.
pub trait Agent: Send {
    fn running(&self) -> bool;
    /// Start the agent on `host`. Messages arrive through `sink`.
    fn start(&mut self, host: &dyn Host, options: &StartOptions, sink: Sink) -> anyhow::Result<()>;
    fn send(&mut self, text: &str) -> std::io::Result<()>;
    /// Answer a permission prompt; `input` is what to run with when allowed.
    fn answer_permission(&mut self, request_id: &str, allow: bool, input: &Value) -> std::io::Result<()>;
    /// Stop the current turn, keeping the process.
    fn interrupt(&mut self) -> std::io::Result<()>;
    fn stop(&mut self);
}

/// The optional features a backend has; the window hides what is missing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Skills (and similar files) it reads from the project and the user's folder.
    pub skills: bool,
    /// Earlier sessions of a folder it can list and resume.
    pub earlier_sessions: bool,
    /// Resuming a session as a copy.
    pub fork: bool,
    /// Subagents it reports.
    pub subagents: bool,
    /// Asking before acting, answered from the window.
    pub permission_prompts: bool,
    /// A permission mode that accepts edits without asking (its name).
    pub accept_edits_mode: Option<String>,
    /// Models it can be asked to use (empty: its default only).
    pub models: Vec<String>,
    /// Its own terminal interface can run a session ([`Backend::terminal_program`]) while the
    /// window follows the session's file ([`Backend::follow_session`]).
    pub terminal_view: bool,
}

/// A session file being followed; dropping it stops the follower.
pub struct Follow(pub Arc<AtomicBool>);

impl Follow {
    pub fn new() -> (Self, Arc<AtomicBool>) {
        let stop = Arc::new(AtomicBool::new(false));
        (Self(stop.clone()), stop)
    }
}

impl Drop for Follow {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// An earlier session of a folder.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionInfo {
    pub id: String,
    pub path: PathBuf,
    /// The first thing the person asked.
    pub title: String,
    /// ISO time of the last message.
    pub last_activity: Option<String>,
    pub messages: usize,
    /// Seconds since the epoch, when known.
    pub modified: Option<u64>,
}

/// An agent process running on this machine, outside this window or in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Running {
    pub pid: u32,
    /// The folder it works in.
    pub cwd: PathBuf,
    /// The session it named on its command line, when it did.
    pub session: Option<String>,
    /// When it started, as `HH:MM` local time.
    pub since: Option<String>,
}

/// A kind of agent. Optional features have defaults that do nothing; [`Backend::capabilities`]
/// says which are real.
pub trait Backend: Send + Sync {
    /// A stable id, saved with sessions ("claude-code").
    fn id(&self) -> &'static str;
    /// Shown in the window ("Claude Code").
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    fn new_agent(&self) -> Box<dyn Agent>;

    /// Skills and similar files relevant to `project`.
    fn skills(&self, _host: &dyn Host, _project: &Path) -> Catalog {
        Catalog::default()
    }
    /// Create a project skill from a template; returns the file.
    fn create_skill(&self, _host: &dyn Host, _project: &Path, _name: &str) -> std::io::Result<PathBuf> {
        Err(std::io::Error::other("this agent has no skills"))
    }
    /// Earlier sessions of `project`, newest first.
    fn earlier_sessions(&self, _host: &dyn Host, _project: &Path, _max: usize) -> Vec<SessionInfo> {
        Vec::new()
    }
    /// How many sessions `project` has and when the newest changed (cheap: no file contents).
    fn session_count(&self, _host: &dyn Host, _project: &Path) -> (usize, Option<u64>) {
        (0, None)
    }
    /// A session's history as a chat.
    fn load_session(&self, _host: &dyn Host, _project: &Path, _id: &str) -> std::io::Result<Conversation> {
        Err(std::io::Error::other("this agent cannot resume sessions"))
    }
    /// The agent's own terminal interface for session `id` (`new`: a session not started yet,
    /// to be created under that id). Run in a pseudo-terminal while the window follows the file.
    fn terminal_program(&self, _options: &StartOptions, _id: &str, _new: bool) -> Option<crate::host::ProgramSpec> {
        None
    }
    /// Follow session `id` of `project` from now on, off the UI thread: what is added to it
    /// arrives through `sink` as events, until the returned handle is dropped.
    fn follow_session(&self, _host: Arc<dyn Host>, _project: &Path, _id: &str, _sink: Sink) -> Option<Follow> {
        None
    }
    /// What to type into the terminal interface to leave it cleanly, in pieces sent a moment
    /// apart.
    fn terminal_exit_input(&self) -> Vec<String> {
        Vec::new()
    }
    /// Every agent process of this kind on this machine except this window's own (children of
    /// `own_pid`). Read-only: nothing is signalled. Empty for a remote host.
    fn running(&self, _host: &dyn Host, _own_pid: u32) -> Vec<Running> {
        Vec::new()
    }
    /// Agent processes elsewhere on this machine working in `project`, as `(pid, session id)`.
    /// Read-only: nothing is signalled.
    fn running_elsewhere(&self, _host: &dyn Host, _project: &Path, _sessions: &[SessionInfo], _own_pid: u32) -> Vec<(u32, String)> {
        Vec::new()
    }
}

/// The backends a window can start sessions with.
#[derive(Clone, Default)]
pub struct Registry {
    backends: Vec<Arc<dyn Backend>>,
}

impl Registry {
    pub fn new(backends: Vec<Arc<dyn Backend>>) -> Self {
        Self { backends }
    }

    pub fn all(&self) -> &[Arc<dyn Backend>] {
        &self.backends
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Backend>> {
        self.backends.iter().find(|b| b.id() == id).cloned()
    }

    /// The first backend: the default for new sessions.
    pub fn default_backend(&self) -> Option<Arc<dyn Backend>> {
        self.backends.first().cloned()
    }
}
