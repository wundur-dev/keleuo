//! One chat session: an agent (from its backend) and the conversation it feeds. No drawing here.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::prelude::*;
use gpui_kit::*;
use workbench_core::agent::{Agent, Backend, Message, StartOptions};
use workbench_core::conversation::Conversation;
use workbench_core::host::Host;

/// Something the rest of the window should react to.
#[derive(Clone, Debug)]
pub enum SessionEvent {
    /// The agent wrote or edited this file.
    FileChanged(PathBuf),
    TurnEnded,
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
        self._reader = Some(
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
            }),
        );
        Ok(())
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

    pub fn stop(&mut self) {
        self.agent.stop();
        self._reader = None;
        self.chat.process_ended();
    }
}

impl Drop for AgentSession {
    fn drop(&mut self) {
        self.stop();
    }
}
