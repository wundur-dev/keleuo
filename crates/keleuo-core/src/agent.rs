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

/// A known way a tool call fails for no good reason (the agent then has to try again), with
/// fixes in the agent's own terms that keep it from happening again.
#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    /// Which known issue (the backend's name for it, for [`Backend::apply_fix`]).
    pub kind: String,
    pub title: String,
    /// What happened, in a sentence or two.
    pub detail: String,
    pub fixes: Vec<IssueFix>,
}

/// One way to fix an [`Issue`]: what it is called and exactly what it would write where.
#[derive(Clone, Debug, PartialEq)]
pub struct IssueFix {
    pub id: String,
    pub label: String,
    pub preview: String,
}

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
    /// A subagent runs in the background: its tool call returns at once (`agent_id`: the id its
    /// report will come back under), and it ends with [`Event::SubagentEnded`] later.
    SubagentBackground { id: String, agent_id: Option<String> },
    /// A message from another agent: a background subagent's report, or a teammate's.
    AgentMessage { from: String, text: String },
    /// A preview of the reply being written, read off the agent's screen before its record has
    /// it: shown until the finished text arrives ([`Event::Text`]), and dropped if a tool call
    /// or the end of the turn comes instead.
    TextPreview(String),
    /// What a running tool has printed so far (its latest part), while it runs.
    ToolOutput { id: String, text: String },
    /// A tool call failed in a known way (see [`Issue`]).
    ToolIssue { id: String, issue: Issue },
    /// The person stopped the turn: it is over, with no answer to come.
    Interrupted,
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
    /// Spend worked out from token counts and published prices (the agent reported none).
    CostEstimate { usd: f64 },
    /// Images the person sent with their last message (stored files).
    UserImages(Vec<PathBuf>),
}

/// The permission modes as `(mode, what the status line says)`, in the order the mode key
/// cycles them, and that key.
pub type ModeCycle = (Vec<(String, String)>, Vec<u8>);

/// How to start an agent's interface on a session: continue it (`--resume`) only when it has
/// a conversation to continue; a session that has none yet starts afresh under its id, and a
/// fork of one with nothing to copy starts afresh too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Launch {
    /// A new conversation under this id.
    New,
    /// Continue the conversation.
    Resume,
    /// Continue a copy of it.
    Fork,
}

impl Launch {
    pub fn decide(has_id: bool, has_messages: bool, fork: bool) -> Self {
        match (has_id && has_messages, fork) {
            (false, _) => Launch::New,
            (true, true) => Launch::Fork,
            (true, false) => Launch::Resume,
        }
    }
}

/// What the agent's own interface reports through the window's hooks, in neutral terms (see
/// [`Backend::hook_settings`] and [`Backend::parse_hook`]).
#[derive(Clone, Debug, PartialEq)]
pub enum Hook {
    /// The interface is up on this session (its id and file).
    Started { session_id: Option<String>, transcript: Option<PathBuf> },
    /// The person's message went in (typed in the window or in the terminal). Empty for a
    /// prompt the interface made itself (a subagent's report, a task notice): the session file
    /// has those.
    Prompt(String),
    /// A tool is about to run.
    ToolStarting { id: Option<String>, tool: String, input: Value },
    /// The agent asks before running a tool. The hook waits for [`Backend::hook_reply`]; the
    /// interface asks in the terminal at the same time, and whichever answers first counts.
    Permission { tool: String, summary: String, description: Option<String>, input: Value },
    /// A tool finished, failed or was refused.
    ToolFinished { id: Option<String>, tool: String, input: Value },
    /// The turn is over: the agent waits for the person.
    TurnEnded,
    /// The interface wants the person's attention.
    Attention(String),
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
    /// Extra settings for the agent's own interface (a file with the window's hooks).
    pub settings: Option<PathBuf>,
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
    /// Its own terminal interface drives the session for the session's whole life
    /// ([`Backend::terminal_program`]): the Terminal view shows it, and Chat is drawn from the
    /// session's file ([`Backend::follow_session`]) and types into it. Without it, the agent
    /// runs behind the window in its structured mode ([`Agent`]) and there is only Chat.
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

/// A session that is, or may be, open in an agent process outside this window. Such a
/// session must not be continued here as it is: two processes would write one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveSession {
    pub id: String,
    /// The process, when one was found.
    pub pid: Option<u32>,
    pub reason: LiveReason,
}

/// How a live session was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveReason {
    /// A process names the session on its command line.
    CommandLine,
    /// A process works in the folder (which of its sessions it writes is a guess).
    Folder,
    /// No process found, but the session file changed a moment ago.
    RecentWrite,
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
    /// A session's history as a chat. Long histories may leave parts as
    /// [`crate::conversation::Item::Earlier`], read with [`Backend::load_segment`].
    fn load_session(&self, _host: &dyn Host, _project: &Path, _id: &str) -> std::io::Result<Conversation> {
        Err(std::io::Error::other("this agent cannot resume sessions"))
    }
    /// Whether session `id` of `project` has a conversation the agent can continue (its file
    /// exists and holds a message). Until then a restart must start it afresh under the same
    /// id, not continue it.
    fn session_has_messages(&self, _host: &dyn Host, _project: &Path, _id: &str) -> bool {
        true
    }
    /// The items of an [`crate::conversation::Item::Earlier`] part of session `id`.
    fn load_segment(&self, _host: &dyn Host, _project: &Path, _id: &str, _from: u64, _to: u64) -> std::io::Result<Vec<crate::conversation::Item>> {
        Err(std::io::Error::other("this agent cannot read earlier parts"))
    }
    /// The agent's own terminal interface for session `id` (`new`: a session not started yet,
    /// to be created under that id; `options.fork`: continue `id` in a copy). Run in a
    /// pseudo-terminal while the window follows the file.
    fn terminal_program(&self, _options: &StartOptions, _id: &str, _new: bool) -> Option<crate::host::ProgramSpec> {
        None
    }
    /// Follow session `id` of `project` from now on, off the UI thread: what is added to it
    /// arrives through `sink` as events, until the returned handle is dropped.
    fn follow_session(&self, _host: Arc<dyn Host>, _project: &Path, _id: &str, _sink: Sink) -> Option<Follow> {
        None
    }
    /// What to type into the terminal interface to send `text` as the person's message, in
    /// pieces written a moment apart. `bracketed`: the interface asked for bracketed paste.
    fn terminal_message(&self, text: &str, bracketed: bool) -> Vec<Vec<u8>> {
        // A command (`/model opus`) is typed, so the interface takes it as a command; a paste
        // of it would be plain text.
        let command = text.starts_with('/') && !text.contains('\n');
        let body = if bracketed && !command { format!("\x1b[200~{text}\x1b[201~") } else { text.replace('\n', " ") };
        vec![body.into_bytes(), b"\r".to_vec()]
    }
    /// What to type to attach an image file to the message being written (before its text),
    /// or `None` when the interface cannot take images that way.
    fn terminal_attach(&self, _path: &Path, _bracketed: bool) -> Option<Vec<u8>> {
        None
    }
    /// The key that makes the interface paste an image from the clipboard itself.
    fn terminal_paste_image(&self) -> Option<Vec<u8>> {
        None
    }
    /// The command that switches the model (`/model <name>`), when there is one.
    fn model_command(&self, _model: &str) -> Option<String> {
        None
    }
    /// The permission modes, in the order the interface's mode key cycles through them, as
    /// `(mode name, what its status line says)`; and that key.
    fn mode_cycle(&self) -> Option<ModeCycle> {
        None
    }
    /// The permission mode a hook call reports the interface is in.
    fn hook_mode(&self, _payload: &Value) -> Option<String> {
        None
    }
    /// What to type to stop the current turn.
    fn terminal_interrupt(&self) -> Vec<u8> {
        b"\x1b".to_vec()
    }
    /// The contents of a settings file that makes the terminal interface run `command(event)`
    /// on each of its hook events, or `None` when it has no hooks.
    fn hook_settings(&self, _command: &dyn Fn(&str) -> String) -> Option<String> {
        None
    }
    /// One hook call (`event` as named in [`Backend::hook_settings`], and what the interface
    /// passed it), in neutral terms.
    fn parse_hook(&self, _event: &str, _payload: &Value, _cwd: &str) -> Option<Hook> {
        None
    }
    /// Whether `host`'s agent is billed per use (an API key) rather than by a subscription
    /// plan, as far as can be told without reading any credential. `None`: cannot tell.
    fn billed_per_use(&self, _host: &dyn Host, _project: &Path) -> Option<bool> {
        None
    }
    /// The context window of `model`, in tokens, when known.
    fn context_window(&self, _model: &str) -> Option<u64> {
        None
    }
    /// Whether the agent has been told it may work in `project` (its own record), when known.
    fn folder_trusted(&self, _host: &dyn Host, _project: &Path) -> Option<bool> {
        None
    }
    /// What a waiting permission hook answers.
    fn hook_reply(&self, _allow: bool) -> String {
        String::new()
    }
    /// Every agent process of this kind on this machine except this window's own (children of
    /// `own_pid`). Read-only: nothing is signalled. Empty for a remote host.
    fn running(&self, _host: &dyn Host, _own_pid: u32) -> Vec<Running> {
        Vec::new()
    }
    /// Sessions of `project` that are, or may be, open in an agent process outside this window
    /// (`own_pid`'s children and the sessions in `own` are this window's). Read-only: nothing
    /// is signalled.
    fn live_sessions(&self, _host: &dyn Host, _project: &Path, _sessions: &[SessionInfo], _own_pid: u32, _own: &[String]) -> Vec<LiveSession> {
        Vec::new()
    }
    /// Whether session `id` of `project` is, or may be, open outside this window, checked now
    /// (just before it would be continued here).
    fn session_live(&self, _host: &dyn Host, _project: &Path, _id: &str, _own_pid: u32) -> Option<LiveSession> {
        None
    }
    /// The folder `old` was renamed to `new`: move what the agent keeps for it (its sessions,
    /// its notes) so they belong to the new name. Nothing to move by default.
    fn folder_renamed(&self, _host: &dyn Host, _old: &Path, _new: &Path) -> std::io::Result<()> {
        Ok(())
    }
    /// The reply being written, read off the agent's own interface (its terminal's lines,
    /// scrollback first, and its width), for a preview in the chat. `None` when there is none,
    /// or the screen does not look as expected.
    fn reply_preview(&self, _lines: &[String], _columns: usize) -> Option<String> {
        None
    }
    /// Whether a message typed into the agent's own interface still sits in its input box,
    /// unsent (its Enter was lost), from the terminal's lines.
    fn message_unsent(&self, _lines: &[String], _text: &str) -> bool {
        false
    }
    /// Apply fix `fix` of the known issue `kind` (see [`Issue`]) for `project`; what was done,
    /// in a sentence.
    fn apply_fix(&self, _host: &dyn Host, _project: &Path, _kind: &str, _fix: &str) -> std::io::Result<String> {
        Err(std::io::Error::other("this agent has no fixes"))
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

#[cfg(test)]
mod launch_tests {
    use super::Launch;

    /// Every relaunch (model, mode, view, take over, after an exit) before the first message:
    /// the session has an id but no conversation, so it starts afresh under that id.
    #[test]
    fn a_restart_before_the_first_message_starts_afresh() {
        assert_eq!(Launch::decide(true, false, false), Launch::New);
        // A fork asked of a session with nothing in it yet starts afresh too.
        assert_eq!(Launch::decide(true, false, true), Launch::New);
        // No id yet: new.
        assert_eq!(Launch::decide(false, false, false), Launch::New);
        // With a conversation: continue it, or a copy.
        assert_eq!(Launch::decide(true, true, false), Launch::Resume);
        assert_eq!(Launch::decide(true, true, true), Launch::Fork);
    }
}
