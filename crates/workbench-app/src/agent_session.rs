//! One chat session: an agent (from its backend) and the conversation it feeds. No drawing here.
//!
//! A session shows in one of two views. **Chat**: the backend's agent runs behind the window
//! and its events build the chat. **Terminal**: the agent's own interface runs in a pseudo-
//! terminal on the same conversation, and the window follows the session's file to keep the
//! chat state (subagents, edits, tokens, status) current. Only one process drives a session:
//! switching ends one before starting the other, and only between turns.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::prelude::*;
use gpui_kit::*;
use workbench_core::agent::{Agent, Backend, Follow, Message, StartOptions};
use workbench_core::conversation::Conversation;
use workbench_core::host::Host;

use crate::terminal_view::TerminalView;

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

/// The terminal view's process and the follower of its session file.
struct TerminalRun {
    view: Entity<TerminalView>,
    _follow: Option<Follow>,
    _reader: Task<()>,
    _exit: Subscription,
}

pub struct AgentSession {
    host: Arc<dyn Host>,
    pub cwd: PathBuf,
    pub chat: Conversation,
    pub backend: Arc<dyn Backend>,
    agent: Box<dyn Agent>,
    /// `None` is the backend's default mode (it asks); otherwise its own mode name.
    pub permission_mode: Option<String>,
    pub model: Option<String>,
    /// Something happened since the user last looked at this session.
    pub unseen: bool,
    /// The next start forks the resumed session (`--fork-session`).
    fork_next: bool,
    /// Waiting for an agent process elsewhere (this pid) to exit before resuming here.
    pub waiting_for: Option<u32>,
    _reader: Option<Task<()>>,
    pub view: View,
    terminal: Option<TerminalRun>,
    /// Back to Chat once the terminal's turn ends.
    pub chat_when_idle: bool,
    /// Leaving the terminal: waiting for it to exit.
    leaving: Option<Task<()>>,
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
            permission_mode: None,
            model: std::env::var("WORKBENCH_MODEL")
                .ok()
                .filter(|m| !m.is_empty()),
            unseen: false,
            fork_next: false,
            waiting_for: None,
            _reader: None,
            view: View::Chat,
            terminal: None,
            chat_when_idle: false,
            leaving: None,
        }
    }

    /// A session continuing an earlier one, its history already rebuilt from the file.
    /// `fork`: continue in a copy (the original runs elsewhere).
    pub fn resumed(
        host: Arc<dyn Host>,
        backend: Arc<dyn Backend>,
        cwd: PathBuf,
        chat: Conversation,
        fork: bool,
    ) -> Self {
        let mut s = Self::new(host, backend, cwd);
        s.chat = chat;
        s.fork_next = fork;
        s.chat.push_notice(if fork {
            "Forked: the next message continues a copy of this conversation here; the original keeps running."
        } else {
            "Resumed: the next message continues this conversation."
        });
        s
    }

    fn start(&mut self, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let options = StartOptions {
            cwd: self.cwd.clone(),
            model: self.model.clone(),
            permission_mode: self.permission_mode.clone(),
            resume: self.chat.session_id.clone(),
            fork: std::mem::take(&mut self.fork_next),
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
        {
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
    }

    fn handle(&mut self, message: Message, cx: &mut Context<Self>) {
        match message {
            Message::Event(event) => {
                let applied = self.chat.apply(event);
                if let Some(path) = applied.file_changed {
                    cx.emit(SessionEvent::FileChanged(path));
                }
                if applied.turn_ended {
                    cx.emit(SessionEvent::TurnEnded);
                    if self.chat_when_idle && self.view == View::Terminal {
                        self.chat_when_idle = false;
                        self.show_chat(cx);
                    }
                }
            }
            Message::Log(line) => {
                if !line.trim().is_empty() {
                    self.chat
                        .push_notice(format!("{}: {line}", self.backend.name()));
                }
            }
            Message::Closed => {
                self.agent.stop();
                if self.chat.busy {
                    self.chat.push_error(format!(
                        "{} stopped before the turn finished.",
                        self.backend.name()
                    ));
                }
                self.chat.process_ended();
            }
        }
    }

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
        if !self.agent.running()
            && let Err(e) = self.start(cx)
        {
            self.chat.push_error(e.to_string());
            cx.notify();
            return;
        }
        self.chat.push_user(text);
        if let Err(e) = self.agent.send(text) {
            self.chat
                .push_error(format!("Could not send to {}: {e}", self.backend.name()));
        }
        cx.notify();
    }

    pub fn answer(&mut self, request_id: &str, allow: bool, cx: &mut Context<Self>) {
        if let Some(input) = self.chat.answer_permission(request_id, allow) {
            let _ = self.agent.answer_permission(request_id, allow, &input);
        }
        cx.notify();
    }

    /// Answer the oldest open permission prompt (for the control channel).
    pub fn answer_first(&mut self, allow: bool, cx: &mut Context<Self>) {
        let id = self
            .chat
            .pending_permissions()
            .next()
            .map(|p| p.request_id.clone());
        if let Some(id) = id {
            self.answer(&id, allow, cx);
        }
    }

    pub fn interrupt(&mut self, cx: &mut Context<Self>) {
        let _ = self.agent.interrupt();
        cx.notify();
    }

    /// Switch permission mode: the process restarts and the conversation continues (`--resume`).
    pub fn set_permission_mode(&mut self, mode: Option<String>, cx: &mut Context<Self>) {
        if self.permission_mode == mode {
            return;
        }
        self.permission_mode = mode;
        self.stop();
        self.chat.push_notice(format!(
            "Permission mode: {}. Applies from the next message.",
            self.permission_mode
                .as_deref()
                .unwrap_or("ask before acting")
        ));
        cx.notify();
    }

    // ----- views -----

    pub fn terminal_view(&self) -> Option<Entity<TerminalView>> {
        self.terminal.as_ref().map(|t| t.view.clone())
    }

    pub fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        match view {
            View::Chat => self.show_chat(cx),
            View::Terminal => self.show_terminal(cx),
        }
    }

    fn remember_view(&self, cx: &mut Context<Self>) {
        if let Some(id) = &self.chat.session_id {
            let settings = cx.global::<crate::AppSettings>().0.clone();
            settings.borrow_mut().set_view(id, self.view.name());
            let _ = settings.borrow().save();
        }
    }

    /// Open in the view this session was last shown in, else the default for new sessions.
    pub fn apply_remembered_view(&mut self, cx: &mut Context<Self>) {
        let want = cx.global::<crate::AppSettings>().0.borrow().view_for(self.chat.session_id.as_deref()).to_string();
        if want == "terminal" && self.backend.capabilities().terminal_view && !self.fork_next && self.waiting_for.is_none() {
            self.show_terminal(cx);
        }
    }

    /// Hand the conversation to the agent's own interface in a terminal, between turns.
    pub fn show_terminal(&mut self, cx: &mut Context<Self>) {
        if self.view == View::Terminal || !self.backend.capabilities().terminal_view {
            return;
        }
        let refuse = if self.chat.busy {
            Some("A turn is running. Wait for it to finish (or stop it), then switch to Terminal.")
        } else if self.waiting_for.is_some() {
            Some("This conversation is still open in another terminal. It can move here once that one ends.")
        } else if self.fork_next {
            Some("Send the first message here in Chat to make the copy; then Terminal can take it.")
        } else {
            None
        };
        if let Some(why) = refuse {
            self.chat.push_notice(why);
            cx.notify();
            return;
        }
        // The chat's process ends between turns: only one process drives a session.
        self.agent.stop();
        self._reader = None;
        self.chat.process_ended();
        let (id, new) = match self.chat.session_id.clone() {
            Some(id) => (id, false),
            None => (new_session_id(), true),
        };
        let options = StartOptions {
            cwd: self.cwd.clone(),
            model: self.model.clone(),
            permission_mode: self.permission_mode.clone(),
            resume: None,
            fork: false,
        };
        let Some(spec) = self.backend.terminal_program(&options, &id, new) else {
            return;
        };
        let command = self.host.program_command(&spec);
        let view = cx.new(|cx| TerminalView::new(command, cx));
        let (tx, rx) = async_channel::unbounded::<Message>();
        let follow = self.backend.follow_session(
            self.host.clone(),
            &self.cwd,
            &id,
            Arc::new(move |m| {
                let _ = tx.send_blocking(m);
            }),
        );
        let exit = cx.observe(&view, |s, view, cx| {
            if view.read(cx).exited && s.view == View::Terminal {
                // The person left the interface (/exit): back to Chat with what it added.
                s.back_to_chat(cx);
            }
        });
        self.chat.session_id = Some(id);
        self.terminal = Some(TerminalRun { view, _follow: follow, _reader: Self::listen(rx, cx), _exit: exit });
        self.view = View::Terminal;
        self.remember_view(cx);
        cx.notify();
    }

    /// Back to the chat view: once the terminal's turn is over, leave its interface cleanly,
    /// then reload the conversation it added to.
    pub fn show_chat(&mut self, cx: &mut Context<Self>) {
        if self.view == View::Chat || self.leaving.is_some() {
            return;
        }
        let Some(run) = &self.terminal else {
            self.view = View::Chat;
            cx.notify();
            return;
        };
        if run.view.read(cx).exited {
            self.back_to_chat(cx);
            return;
        }
        if self.chat.busy && !self.chat_when_idle {
            // Asked once: wait for the turn. Asked again: go now (the exit input stops it).
            self.chat_when_idle = true;
            self.chat.push_notice("The agent is working in the terminal. Chat takes over when this turn ends (press Chat again to stop it now).");
            cx.notify();
            return;
        }
        self.chat_when_idle = false;
        let pty = run.view.read(cx).pty.clone();
        let input = self.backend.terminal_exit_input();
        self.leaving = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            for piece in input {
                if let Some(pty) = &pty {
                    pty.write(piece.into_bytes());
                }
                cx.background_executor().timer(std::time::Duration::from_millis(400)).await;
            }
            // Give it a few seconds to exit; otherwise ask the person.
            for _ in 0..20 {
                cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                let exited = this
                    .update(cx, |s, cx| s.terminal.as_ref().is_none_or(|t| t.view.read(cx).exited))
                    .unwrap_or(true);
                if exited {
                    let _ = this.update(cx, |s, cx| {
                        s.leaving = None;
                        s.back_to_chat(cx)
                    });
                    return;
                }
            }
            let _ = this.update(cx, |s, cx| {
                s.leaving = None;
                s.chat.push_notice("The terminal did not exit. Finish there (type /exit), then switch to Chat.");
                cx.notify();
            });
        }));
    }

    /// The terminal has exited: drop it and rebuild the chat from the session file, so the
    /// next message continues the conversation in stream mode.
    fn back_to_chat(&mut self, cx: &mut Context<Self>) {
        self.terminal = None;
        self.view = View::Chat;
        self.remember_view(cx);
        cx.notify();
        let Some(id) = self.chat.session_id.clone() else { return };
        let (host, backend, cwd) = (self.host.clone(), self.backend.clone(), self.cwd.clone());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let chat = cx
                .background_spawn(async move { backend.load_session(host.as_ref(), &cwd, &id) })
                .await;
            let _ = this.update(cx, |s, cx| {
                if let Ok(mut chat) = chat {
                    // What the window counted in Chat stays; the terminal does not report cost.
                    chat.total_cost_usd = s.chat.total_cost_usd;
                    chat.process_ended();
                    s.chat = chat;
                }
                s.chat.push_notice("Back in Chat: the next message continues this conversation.");
                cx.notify();
            });
        })
        .detach();
    }

    pub fn stop(&mut self) {
        self.terminal = None;
        self.agent.stop();
        self._reader = None;
        self.chat.process_ended();
    }
}

/// A random version 4 UUID, for a session started in the terminal view.
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
