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
use keleuo_core::agent::{Agent, Backend, Event, Follow, Hook, Launch, Message, StartOptions};
use keleuo_core::conversation::Conversation;
use keleuo_core::host::Host;

use crate::hooks::{self, Registration, Reply};
use crate::terminal_view::{ImageSupport, Placeholder, TerminalEvent, TerminalView};
use std::rc::Rc;

/// What a session has used, as the window shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum Spend {
    /// Dollars (`estimated`: from token counts and published prices).
    Cost { total: f64, turn: Option<f64>, estimated: bool },
    /// How full the context window is after the last turn.
    Context { percent: u32, tokens: u64, window: u64 },
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

pub struct AgentSession {
    host: Arc<dyn Host>,
    /// Opened again at start: its conversation shows, but its agent starts only when it is
    /// needed (a message, or its Terminal shown), so reopening many sessions costs nothing.
    pub dormant: bool,
    /// When the reply preview was last read off the terminal (at most ten times a second).
    preview_read: Option<std::time::Instant>,
    /// Messages the interface reported submitted (its prompt hook), to tell a sent message
    /// from one whose Enter was lost.
    prompts_seen: u64,
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
}

impl EventEmitter<SessionEvent> for AgentSession {}

impl AgentSession {
    /// Apply a fix for a known tool failure (see `Issue`) in this session's folder.
    pub fn apply_fix(&self, kind: &str, fix: &str) -> std::io::Result<String> {
        self.backend.apply_fix(self.host.as_ref(), &self.cwd, kind, fix)
    }

    pub fn new(host: Arc<dyn Host>, backend: Arc<dyn Backend>, cwd: PathBuf) -> Self {
        Self {
            agent: backend.new_agent(),
            backend,
            host,
            dormant: false,
            preview_read: None,
            prompts_seen: 0,
            chat: Conversation::new(cwd.to_string_lossy().to_string()),
            cwd,
            permission_mode: keleuo_core::env_var("PERMISSION_MODE").ok().filter(|m| !m.is_empty()),
            model: keleuo_core::env_var("MODEL").ok().filter(|m| !m.is_empty()),
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
            per_use: None,
            asking: None,
            trusted: None,
            live_mode: None,
            skip_images: 0,
            pasted_images: Vec::new(),
            restart_after_turn: false,
            restarting: None,
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

    /// Read an earlier part of this session's file (off the UI thread).
    pub fn earlier_loader(&self) -> Option<impl FnOnce(u64, u64) -> std::io::Result<Vec<keleuo_core::conversation::Item>> + Send + 'static> {
        let (host, backend, cwd) = (self.host.clone(), self.backend.clone(), self.cwd.clone());
        let id = self.chat.session_id.clone()?;
        Some(move |from, to| backend.load_segment(host.as_ref(), &cwd, &id, from, to))
    }

    /// Messages waiting for the interface to be up.
    pub fn queued_count(&self) -> usize {
        self.queued.len()
    }

    /// Whether the agent's own terminal interface drives this session (Chat is then a view of
    /// its session file). Local folders only: its hooks reach the window over a local socket.
    pub fn interactive(&self) -> bool {
        self.backend.capabilities().terminal_view && !self.host.is_remote()
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
        if !self.interactive() {
            return;
        }
        let want = cx.global::<crate::AppSettings>().0.borrow().view_for(self.chat.session_id.as_deref()).to_string();
        if want == "terminal" {
            self.view = View::Terminal;
        }
        if self.waiting_for.is_none() && !self.dormant {
            self.start_terminal(cx);
        }
    }

    /// The terminal drew: while the agent works and Chat shows, read the reply it is writing
    /// off the screen as a preview (the record has it only once it is finished).
    fn read_preview(&mut self, view: &Entity<TerminalView>, cx: &mut Context<Self>) {
        if !self.chat.busy || self.view != View::Chat {
            return;
        }
        let now = std::time::Instant::now();
        if self.preview_read.is_some_and(|t| now.duration_since(t) < PREVIEW_EVERY) {
            return;
        }
        self.preview_read = Some(now);
        let Some((lines, columns)) = view.read(cx).recent_lines(PREVIEW_LINES) else { return };
        if let Some(text) = self.backend.reply_preview(&lines, columns) {
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
        cx.notify();
    }

    /// Cost when it is billed per use (or the setting says always), else how full the context
    /// is. `None` before there is anything to show.
    pub fn spend(&self, cx: &App) -> Option<Spend> {
        let setting = cx.global::<crate::AppSettings>().0.borrow().cost_display.clone();
        let cost = match setting.as_str() {
            "always" => true,
            "never" => false,
            _ => self.per_use == Some(true),
        };
        if cost {
            return (self.chat.total_cost_usd > 0.0).then_some(Spend::Cost {
                total: self.chat.total_cost_usd,
                turn: self.chat.last_turn_cost_usd,
                estimated: self.chat.cost_estimated,
            });
        }
        let tokens = self.chat.last_turn_tokens?.0;
        let model = self.chat.model.clone().or_else(|| self.model.clone())?;
        let window = self.backend.context_window(&model)?;
        Some(Spend::Context { percent: ((tokens * 100) / window.max(1)) as u32, tokens, window })
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
        let options = StartOptions {
            cwd: self.cwd.clone(),
            model: self.model.clone(),
            permission_mode: self.permission_mode.clone(),
            resume: None,
            fork,
            settings: self.hooks.as_ref().map(|h| h.settings.clone()),
        };
        let Some(spec) = self.backend.terminal_program(&options, &id, new) else { return };
        if crate::perf::enabled() || keleuo_core::env_var("LOG_LAUNCH").is_ok() {
            eprintln!("keleuo: launch {} {}", spec.program, spec.args.iter().filter(|a| !a.ends_with(".json")).cloned().collect::<Vec<_>>().join(" "));
        }
        let command = self.host.program_command(&spec);
        let placeholder = Placeholder {
            lines: last_lines(&self.chat, 12),
            label: format!("Starting {}...", self.backend.name()),
        };
        let weak = cx.entity().downgrade();
        let images = self.backend.terminal_paste_image().map(|paste_key| {
            let (w1, w2) = (weak.clone(), weak.clone());
            ImageSupport {
                paste_key,
                pasted: Rc::new(move |bytes, ext, cx| {
                    let _ = w1.update(cx, |s, _| {
                        let dir = keleuo_core::images::session_dir(s.chat.session_id.as_deref());
                        if let Some(path) = dir.and_then(|d| keleuo_core::images::store(&d, &bytes, ext).ok()) {
                            s.pasted_images.push(path);
                        }
                    });
                }),
                image: Rc::new(move |n, cx| w2.upgrade().and_then(|s| s.read(cx).image_n(n))),
            }
        });
        let view = cx.new(|cx| {
            let mut v = TerminalView::new(command, cx);
            v.placeholder = Some(placeholder);
            v.images = images;
            v
        });
        self.pasted_images.clear();
        let exit = cx.observe(&view, |s, view, cx| {
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
        let Some(view) = self.terminal_view() else { return };
        let Some(pty) = view.read(cx).pty.clone() else { return };
        let bracketed = view.read(cx).bracketed_paste();
        let pieces: Vec<Vec<u8>> = images.iter().filter_map(|p| self.backend.terminal_attach(p, bracketed)).collect();
        self.pasted_images.extend(images.iter().cloned());
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
    pub fn set_model(&mut self, model: &str, cx: &mut Context<Self>) {
        self.model = Some(model.to_string());
        if self.interactive() && self.running(cx) {
            if self.chat.busy {
                self.restart_after_turn = true;
                self.chat.push_notice(format!("Model: {model}. Applies when this turn ends."));
            } else {
                self.stop(cx);
                self.restarting = Some(format!("with {model}"));
                self.start_terminal(cx);
                self.chat.push_notice(format!("Model: {model}."));
            }
        } else {
            self.stop(cx);
            self.chat.push_notice(format!("Model: {model}. Applies from the next message."));
        }
        cx.notify();
    }

    /// Switch the permission mode. With the interface running, press its mode key until its
    /// status line shows the mode (as a person would); otherwise restart with it.
    pub fn set_mode(&mut self, mode: Option<String>, cx: &mut Context<Self>) {
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
                    Ok(true) => pty.write(b"\r".to_vec()),
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
                self.prompts_seen += 1;
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
                }
                if std::mem::take(&mut self.restart_after_turn) {
                    self.stop(cx);
                    self.restarting = self.model.as_ref().map(|m| format!("with {m}"));
                    self.start_terminal(cx);
                }
            }
            Hook::Attention(_) => self.unseen = true,
        }
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
                    && let Some(i) = self.echo.iter().position(|e| *e == plain(text))
                {
                    // Shown already, when it was sent.
                    self.echo.remove(i);
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
        self.send_with(text, Vec::new(), cx);
    }

    /// Send a user turn with images (files).
    pub fn send_with(&mut self, text: &str, images: Vec<PathBuf>, cx: &mut Context<Self>) {
        let text = text.trim();
        if text.is_empty() && images.is_empty() {
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
            if !text.is_empty() {
                self.chat.push_user(text);
                self.push_echo(text.to_string());
            }
            if !images.is_empty() {
                self.chat.apply(Event::UserImages(images.clone()));
                self.skip_images += 1;
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
        if !images.is_empty() {
            self.chat.push_notice(format!("{} cannot take images here; sent the text only.", self.backend.name()));
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
        self.stop(cx);
        if now {
            self.restarting = Some("in the new permission mode".into());
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

    /// End the session's agent: its process (the terminal interface is hung up on, not just
    /// let go: other handles may keep its view alive) and the following of its file.
    pub fn stop(&mut self, cx: &mut App) {
        if let Some(run) = self.terminal.take()
            && let Some(pty) = run.view.read(cx).pty.clone()
        {
            pty.hang_up();
        }
        self.release();
    }

    /// Let go of the agent and the file following (all a drop can do, with no app at hand).
    fn release(&mut self) {
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

/// A message as compared with its copy in the session file: without the interface's image
/// markers (`[Image #1]`) and outer spaces.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
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
