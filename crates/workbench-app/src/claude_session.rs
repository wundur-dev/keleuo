//! The Claude Code process for the open project and the chat state it feeds. No drawing here.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::prelude::*;
use gpui_kit::*;
use workbench_core::claude::{Message, Options, Process};
use workbench_core::conversation::Conversation;
use workbench_core::host::Host;

/// Something the rest of the window should react to.
#[derive(Clone, Debug)]
pub enum SessionEvent {
    /// Claude wrote or edited this file.
    FileChanged(PathBuf),
    TurnEnded,
}

pub struct ClaudeSession {
    host: Arc<dyn Host>,
    pub cwd: PathBuf,
    pub chat: Conversation,
    process: Option<Process>,
    /// `None` is Claude's default mode (it asks); `Some("acceptEdits")` etc.
    pub permission_mode: Option<String>,
    pub model: Option<String>,
    _reader: Option<Task<()>>,
}

impl EventEmitter<SessionEvent> for ClaudeSession {}

impl ClaudeSession {
    pub fn new(host: Arc<dyn Host>, cwd: PathBuf) -> Self {
        Self {
            host,
            chat: Conversation::new(cwd.to_string_lossy().to_string()),
            cwd,
            process: None,
            permission_mode: None,
            model: std::env::var("WORKBENCH_MODEL")
                .ok()
                .filter(|m| !m.is_empty()),
            _reader: None,
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let options = Options {
            program: None,
            cwd: self.cwd.clone(),
            model: self.model.clone(),
            permission_mode: self.permission_mode.clone(),
            resume: self.chat.session_id.clone(),
        };
        let spec = self.host.claude_command(&options);
        let (tx, rx) = async_channel::unbounded::<Message>();
        let process = Process::spawn(&spec, move |m| {
            let _ = tx.send_blocking(m);
        })?;
        self.process = Some(process);
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
            Message::Stderr(line) => {
                if !line.trim().is_empty() {
                    self.chat.push_notice(format!("claude: {line}"));
                }
            }
            Message::Closed => {
                if let Some(mut p) = self.process.take() {
                    p.kill();
                }
                if self.chat.busy {
                    self.chat
                        .push_error("Claude Code stopped before the turn finished.");
                }
                self.chat.process_ended();
            }
        }
    }

    /// Send a user turn, starting Claude Code when needed.
    pub fn send(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if self.process.is_none()
            && let Err(e) = self.start(cx)
        {
            self.chat.push_error(e.to_string());
            cx.notify();
            return;
        }
        self.chat.push_user(text);
        if let Some(p) = self.process.as_mut()
            && let Err(e) = p.send_user(text)
        {
            self.chat
                .push_error(format!("Could not send to Claude Code: {e}"));
        }
        cx.notify();
    }

    pub fn answer(&mut self, request_id: &str, allow: bool, cx: &mut Context<Self>) {
        if let Some(line) = self.chat.answer_permission(request_id, allow)
            && let Some(p) = self.process.as_mut()
        {
            let _ = p.send_line(&line);
        }
        cx.notify();
    }

    pub fn interrupt(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.process.as_mut() {
            let _ = p.interrupt();
        }
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
        if let Some(mut p) = self.process.take() {
            p.kill();
        }
        self._reader = None;
        self.chat.process_ended();
    }

    pub fn new_conversation(&mut self, cx: &mut Context<Self>) {
        self.stop();
        self.chat = Conversation::new(self.cwd.to_string_lossy().to_string());
        cx.notify();
    }
}

impl Drop for ClaudeSession {
    fn drop(&mut self) {
        self.stop();
    }
}
