//! The window: a session list at the far left and the shown session beside it. Each session
//! is a whole workspace (folder, one agent conversation, terminals, files, skills, dock
//! layout). Selecting a session switches everything; hidden sessions keep running.
//!
//! The list's rows: a status dot (working, needs permission, unseen, idle), the name (rename
//! in place), folder and git branch, a worktree badge, the last thing that happened and the
//! cost so far. Rows reorder by dragging. Cmd+Shift+B folds the list to a rail of dots and
//! initials. "+ New session" (Cmd+N) opens the directory browser.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, TitleBar, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleion_core::agent::{LiveReason, LiveSession};
use keleion_core::agent::Backend;
use keleion_core::host::{Host, LocalHost};

use crate::AppSettings;
use crate::actions::*;
use crate::agent_session::AgentSession;
use crate::browser::{Browser, BrowserEvent, OpenSession};
use crate::panels::{self, start_tab_rename};
use crate::workspace::{Workspace, WorkspaceEvent};

const LIST_WIDTH: f32 = 250.;
const RAIL_WIDTH: f32 = 44.;

struct Tab {
    number: usize,
    ws: Entity<Workspace>,
}

/// A session row being dragged to a new place.
#[derive(Clone)]
struct SessionDrag {
    number: usize,
    name: SharedString,
}

struct DragLabel(SharedString);

impl Render for DragLabel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme.ring)
            .bg(theme.secondary)
            .text_xs()
            .text_color(theme.foreground)
            .child(self.0.clone())
    }
}

/// What the window shows first.
pub enum Start {
    /// A session in this folder.
    Session(Arc<dyn Host>, PathBuf),
    /// The browser at this folder, with a note (a folder that was not found).
    Browse(Arc<dyn Host>, PathBuf, Option<String>),
}

pub struct Shell {
    tabs: Vec<Tab>,
    active: usize,
    next: usize,
    browser: Option<Entity<Browser>>,
    folded: bool,
    focus: FocusHandle,
    subs: HashMap<EntityId, Vec<Subscription>>,
    _browser_sub: Option<Subscription>,
    /// What the shown browser opened, if anything: a double or triple click (or Enter held)
    /// must open one session, not one per click. Also guards resumes still loading.
    opening: OpenGuard,
    /// The last `earlier` listing (control channel).
    earlier: Vec<(keleion_core::agent::SessionInfo, Option<u32>)>,
}

impl Shell {
    pub fn new(start: Start, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let folded = cx.global::<AppSettings>().0.borrow().sessions_folded;
        let mut this = Self {
            tabs: Vec::new(),
            active: 0,
            next: 1,
            browser: None,
            folded,
            focus: cx.focus_handle(),
            subs: HashMap::new(),
            _browser_sub: None,
            opening: OpenGuard::default(),
            earlier: Vec::new(),
        };
        match start {
            Start::Session(host, root) => {
                let backend = crate::backends::backend(None, cx);
                this.new_session_in(host, root, backend, window, cx);
            }
            Start::Browse(host, dir, note) => this.open_browser(host, dir, note, window, cx),
        }
        this
    }

    pub fn active_workspace(&self) -> Option<Entity<Workspace>> {
        self.tabs.get(self.active).map(|t| t.ws.clone())
    }

    fn open_sessions(&self, cx: &App) -> Vec<OpenSession> {
        self.tabs
            .iter()
            .map(|t| {
                let w = t.ws.read(cx);
                OpenSession { root: w.root().to_path_buf(), id: w.session.read(cx).chat.session_id.clone() }
            })
            .collect()
    }

    // ----- the browser -----

    /// Show the directory browser at `dir` (in place of the shown session, if any).
    pub fn open_browser(&mut self, host: Arc<dyn Host>, dir: PathBuf, note: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        // Unchecked: the browser drops the ones that are gone off the UI thread (a recent folder
        // on a sleeping or unplugged disk must not hold up the window).
        let recent = if host.is_remote() { Vec::new() } else { cx.global::<AppSettings>().0.borrow().recent.clone() };
        let backends = crate::backends::all(cx);
        let open = self.open_sessions(cx);
        let overlay = !self.tabs.is_empty();
        let browser = crate::perf::step("browser", || cx.new(|cx| {
            let mut b = Browser::new(host, dir, recent, backends, open, overlay, window, cx);
            b.note = note;
            b
        }));
        self._browser_sub = Some(cx.subscribe_in(&browser, window, |s, _, ev: &BrowserEvent, window, cx| s.on_browser(ev, window, cx)));
        self.opening.browser_shown();
        self.browser = Some(browser);
        cx.notify();
    }

    /// "+ New session": the browser at the shown session's folder, else the last folder.
    pub fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (host, dir) = match self.active_workspace() {
            Some(w) => {
                let w = w.read(cx);
                (w.host(), w.root().to_path_buf())
            }
            None => (Arc::new(LocalHost) as Arc<dyn Host>, default_folder(cx)),
        };
        self.open_browser(host, dir, None, window, cx);
    }

    fn close_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        self.browser = None;
        self._browser_sub = None;
        if let Some(w) = self.active_workspace() {
            w.update(cx, |w, cx| w.focus_main(window, cx));
        }
        cx.notify();
    }

    fn on_browser(&mut self, ev: &BrowserEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            BrowserEvent::Close => self.close_browser(window, cx),
            BrowserEvent::Switch(id) => {
                if let Some(ix) = self.find_session(id, cx) {
                    self.activate(ix, window, cx);
                }
            }
            BrowserEvent::New { host, root, backend, worktree } => {
                let worktree = worktree.clone();
                if !self.opening.choose() {
                    return;
                }
                if let Some(name) = worktree {
                    if let Err(e) = self.new_session_in_worktree(root.clone(), Some(name), backend.clone(), window, cx) {
                        window.push_notification(format!("Could not make a worktree: {e}"), cx);
                    }
                } else {
                    self.new_session_in(host.clone(), root.clone(), backend.clone(), window, cx);
                }
            }
            BrowserEvent::Resume { host, root, backend, id, fork, wait_for } => {
                if !self.opening.choose() {
                    return;
                }
                self.resume(host.clone(), root.clone(), backend.clone(), id.clone(), *fork, *wait_for, window, cx)
            }
        }
    }

    fn find_session(&self, id: &str, cx: &App) -> Option<usize> {
        self.tabs.iter().position(|t| t.ws.read(cx).session.read(cx).chat.session_id.as_deref() == Some(id))
    }

    /// Continue an earlier session (read off the UI thread). One already open here just shows.
    #[allow(clippy::too_many_arguments)]
    fn resume(
        &mut self,
        host: Arc<dyn Host>,
        root: PathBuf,
        backend: Arc<dyn Backend>,
        id: String,
        fork: bool,
        wait_for: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !fork && let Some(ix) = self.find_session(&id, cx) {
            self.activate(ix, window, cx);
            return;
        }
        // Still loading from an earlier click or command: one session per conversation.
        let key = format!("{}{id}", if fork { "fork:" } else { "" });
        if !self.opening.start(&key) {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let (h, b, r) = (host.clone(), backend.clone(), root.clone());
            let plain = !fork && wait_for.is_none();
            let (live, chat) = cx
                .background_spawn(async move {
                    // Checked now, not when the list was drawn: never a second writer on a
                    // session open elsewhere.
                    let live = if plain { b.session_live(h.as_ref(), &r, &id, std::process::id()) } else { None };
                    let started = std::time::Instant::now();
                    let chat = if live.is_some() { None } else { Some(b.load_session(h.as_ref(), &r, &id)) };
                    if crate::perf::enabled() {
                        eprintln!("keleion: session read in {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
                    }
                    (live, chat)
                })
                .await;
            let _ = this.update_in(cx, |s, window, cx| {
                s.opening.finish(&key);
                match (live, chat) {
                    (Some(live), _) => s.refused(live, window, cx),
                    (None, Some(chat)) => s.opened(chat, host, root, backend, fork, wait_for, window, cx),
                    (None, None) => {}
                }
            });
        })
        .detach();
    }

    /// A plain continue of a session that is (or may be) open elsewhere: say so and offer
    /// only what is safe, in the browser.
    fn refused(&mut self, live: LiveSession, window: &mut Window, cx: &mut Context<Self>) {
        self.opening.browser_shown();
        let why = match (live.reason, live.pid) {
            (LiveReason::RecentWrite, _) | (_, None) => "its file changed seconds ago, so it may be open elsewhere".to_string(),
            (_, Some(pid)) => format!("it is open in another terminal (process {pid})"),
        };
        window.push_notification(
            format!("Not continued here: {why}. Use Fork here, or Take over when it stops."),
            cx,
        );
        if let Some(b) = &self.browser {
            b.update(cx, |b, cx| b.refresh_detail(cx));
        }
        cx.notify();
    }

    #[allow(clippy::too_many_arguments)]
    fn opened(
        &mut self,
        chat: std::io::Result<keleion_core::conversation::Conversation>,
        host: Arc<dyn Host>,
        root: PathBuf,
        backend: Arc<dyn Backend>,
        fork: bool,
        wait_for: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let s = self;
        match chat {
                Ok(chat) => {
                    let mut session = AgentSession::resumed(host.clone(), backend, root.clone(), chat, fork);
                    if let Some(pid) = wait_for {
                        session.waiting_for = Some(pid);
                        session.chat.push_notice(format!(
                            "Waiting for the other terminal (process {pid}) to finish. Quit it there; this session resumes here by itself."
                        ));
                    }
                    s.add_session(host, root, session, None, wait_for, window, cx);
                }
                Err(e) => {
                    // Nothing opened: the browser may be used again.
                    s.opening.browser_shown();
                    window.push_notification(format!("Could not read the session: {e}"), cx)
                }
        }
    }

    // ----- sessions -----

    /// Open a session for `root` with this conversation.
    #[allow(clippy::too_many_arguments)]
    pub fn add_session(
        &mut self,
        host: Arc<dyn Host>,
        root: PathBuf,
        session: AgentSession,
        worktree: Option<String>,
        wait_for: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !host.is_remote() {
            let settings = cx.global::<AppSettings>().0.clone();
            settings.borrow_mut().add_recent(&root);
            let _ = settings.borrow().save();
        }
        let ws = crate::perf::step("workspace", || cx.new(|cx| Workspace::new(host.clone(), root.clone(), session, window, cx)));
        ws.update(cx, |w, _| w.worktree = worktree);
        if let Some(pid) = wait_for {
            let session = ws.read(cx).session.clone();
            crate::agent_side::watch_takeover(session, host, root, pid, cx);
        }
        let session = ws.read(cx).session.clone();
        let subs = vec![
            cx.subscribe_in(&ws, window, |shell, ws, ev: &WorkspaceEvent, window, cx| match ev {
                WorkspaceEvent::NewSession => shell.new_session(window, cx),
                WorkspaceEvent::AgentEdited(path) => shell.edited(ws.clone(), path.clone(), window, cx),
            }),
            // The row's name, dot, activity and cost follow the session.
            cx.observe(&ws, |_, _, cx| cx.notify()),
            cx.observe(&session, |_, _, cx| cx.notify()),
        ];
        let number = self.next;
        self.next += 1;
        self.subs.insert(ws.entity_id(), subs);
        self.tabs.push(Tab { number, ws });
        self.activate(self.tabs.len() - 1, window, cx);
    }

    pub fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active = ix;
        self.browser = None;
        self._browser_sub = None;
        let ws = self.tabs[ix].ws.clone();
        ws.update(cx, |w, cx| {
            w.session.update(cx, |s, _| s.unseen = false);
            w.focus_main(window, cx);
        });
        let title = format!("{} - Keleion", self.name(ix, cx));
        window.set_window_title(&title);
        cx.notify();
    }

    pub fn close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(ix);
        tab.ws.update(cx, |w, cx| w.session.update(cx, |s, _| s.stop()));
        self.subs.remove(&tab.ws.entity_id());
        if self.tabs.is_empty() {
            // Nothing left: back to the browser rather than an empty window.
            let (host, dir) = {
                let w = tab.ws.read(cx);
                (w.host(), w.root().to_path_buf())
            };
            self.open_browser(host, dir, None, window, cx);
            return;
        }
        if self.active > ix || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.activate(self.active, window, cx);
    }

    /// Move the session numbered `number` to place `to`.
    fn reorder(&mut self, number: usize, to: usize, cx: &mut Context<Self>) {
        let Some(from) = self.tabs.iter().position(|t| t.number == number) else { return };
        let shown = self.tabs[self.active].number;
        let tab = self.tabs.remove(from);
        let to = if to > from { to - 1 } else { to }.min(self.tabs.len());
        self.tabs.insert(to, tab);
        self.active = self.tabs.iter().position(|t| t.number == shown).unwrap_or(0);
        cx.notify();
    }

    fn key(&self, ix: usize) -> String {
        panels::session_key(self.tabs[ix].number)
    }

    fn list_name(&self, ix: usize, cx: &App) -> String {
        self.tabs[ix].ws.read(cx).list_name(cx)
    }

    fn name(&self, ix: usize, cx: &App) -> String {
        panels::tab_label(&self.key(ix), self.list_name(ix, cx), cx)
    }

    fn toggle_folded(&mut self, cx: &mut Context<Self>) {
        self.folded = !self.folded;
        let settings = cx.global::<AppSettings>().0.clone();
        settings.borrow_mut().sessions_folded = self.folded;
        let _ = settings.borrow().save();
        cx.notify();
    }

    /// The agent in one session edited a file: warn the other sessions in the same folder that
    /// have it open (sessions in their own git worktree are separate folders and stay quiet).
    fn edited(&mut self, by: Entity<Workspace>, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(by_ix) = self.tabs.iter().position(|t| t.ws == by) else {
            return;
        };
        let by_name = self.name(by_ix, cx);
        let root = by.read(cx).root().to_path_buf();
        for t in &self.tabs {
            if t.ws == by {
                continue;
            }
            let same = t.ws.read(cx).root() == root.as_path() && t.ws.read(cx).has_open(&path, cx);
            if same {
                let name = by_name.clone();
                t.ws.update(cx, |w, cx| w.warn_edited_elsewhere(&path, &name, window, cx));
            }
        }
    }

    pub fn new_session_in(&mut self, host: Arc<dyn Host>, root: PathBuf, backend: Arc<dyn Backend>, window: &mut Window, cx: &mut Context<Self>) {
        let session = AgentSession::new(host.clone(), backend, root.clone());
        self.add_session(host, root, session, None, None, window, cx);
    }

    /// A new session in a git worktree of `root`: a new branch and folder next to the project
    /// (`git worktree add`), so parallel sessions do not edit the same files.
    pub fn new_session_in_worktree(
        &mut self,
        root: PathBuf,
        name: Option<String>,
        backend: Arc<dyn Backend>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let (top, _) = repo_of(&root).ok_or("the folder is not in a git repository")?;
        let name = name.unwrap_or_else(|| suggest_worktree_name(&top));
        let (path, branch) = make_worktree(&top, &name)?;
        let session = AgentSession::new(Arc::new(LocalHost), backend, path.clone());
        self.add_session(Arc::new(LocalHost), path, session, Some(branch), None, window, cx);
        Ok(())
    }

    // ----- control channel -----

    /// Commands for the window (`new`, `tab`, ...); others go to the shown session.
    pub fn command(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<String> {
        let (cmd, arg) = line.trim().split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((line.trim(), ""));
        Some(match cmd {
            "new" => {
                let (host, root) = match self.active_workspace() {
                    Some(w) if arg.is_empty() => {
                        let w = w.read(cx);
                        (w.host(), w.root().to_path_buf())
                    }
                    _ => (Arc::new(LocalHost) as Arc<dyn Host>, PathBuf::from(arg)),
                };
                let b = crate::backends::backend(None, cx);
                self.new_session_in(host, root, b, window, cx);
                "ok".into()
            }
            "new-scripted" => {
                // A session on the scripted test backend (needs KELEION_TEST_BACKENDS=1).
                let Some(b) = crate::backends::all(cx).into_iter().find(|b| b.id() != crate::backends::backend(None, cx).id()) else {
                    return Some("no second backend".into());
                };
                let root = if arg.is_empty() { default_folder(cx) } else { PathBuf::from(arg) };
                self.new_session_in(Arc::new(LocalHost), root, b, window, cx);
                "ok".into()
            }
            "new-worktree" => {
                let Some(root) = self.active_workspace().map(|w| w.read(cx).root().to_path_buf()) else {
                    return Some("no session".into());
                };
                let b = crate::backends::backend(None, cx);
                let name = (!arg.is_empty()).then(|| arg.to_string());
                match self.new_session_in_worktree(root, name, b, window, cx) {
                    Ok(()) => "ok".into(),
                    Err(e) => e,
                }
            }
            "browse" => {
                if arg.is_empty() {
                    self.new_session(window, cx);
                } else {
                    self.open_browser(Arc::new(LocalHost), PathBuf::from(arg), None, window, cx);
                }
                "ok".into()
            }
            "appearance" => {
                crate::theme::set_appearance(arg, cx);
                format!("dark={}", gpui_kit::component::ActiveTheme::theme(&**cx).is_dark())
            }
            "worktree-ask" => {
                // Tests: show the worktree form for the selected folder.
                match self.browser.clone() {
                    Some(b) => {
                        b.update(cx, |b, cx| b.ask_worktree(window, cx));
                        "ok".into()
                    }
                    None => "no browser".into(),
                }
            }
            "close-browser" => {
                self.close_browser(window, cx);
                "ok".into()
            }
            "earlier" => {
                // The shown session's folder's earlier sessions, as the browser lists them (tests).
                let Some(w) = self.active_workspace() else { return Some("no session".into()) };
                let (host, root, backend) = {
                    let w = w.read(cx);
                    (w.host(), w.root().to_path_buf(), w.session.read(cx).backend.clone())
                };
                let sessions = backend.earlier_sessions(host.as_ref(), &root, 20);
                let own: Vec<String> = self.open_sessions(cx).into_iter().filter_map(|o| o.id).collect();
                let live = backend.live_sessions(host.as_ref(), &root, &sessions, std::process::id(), &own);
                self.earlier = sessions
                    .into_iter()
                    .map(|s| {
                        let l = live.iter().find(|l| l.id == s.id);
                        (s, l.map(|l| l.pid.unwrap_or(0)))
                    })
                    .collect();
                self.earlier
                    .iter()
                    .enumerate()
                    .map(|(i, (s, pid))| format!("{}:{}:{}:{}", i + 1, s.messages, if pid.is_some() { "running" } else { "idle" }, s.title))
                    .collect::<Vec<_>>()
                    .join(" | ")
            }
            "resume" | "fork" | "takeover" => {
                let n = arg.parse::<usize>().unwrap_or(1).saturating_sub(1);
                let (Some((s, pid)), Some(w)) = (self.earlier.get(n).cloned(), self.active_workspace()) else {
                    return Some("no such earlier session (run earlier first)".into());
                };
                let (host, root, backend) = {
                    let w = w.read(cx);
                    (w.host(), w.root().to_path_buf(), w.session.read(cx).backend.clone())
                };
                let (fork, wait) = match cmd {
                    "fork" => (true, None),
                    "takeover" => (false, pid.filter(|p| *p != 0)),
                    _ => (false, None),
                };
                self.resume(host, root, backend, s.id, fork, wait, window, cx);
                "ok".into()
            }
            "fold-sessions" => {
                self.toggle_folded(cx);
                format!("folded={}", self.folded)
            }
            "move-session" => {
                // `move-session <from> <to>`, 1-based (tests).
                let mut it = arg.split_whitespace().filter_map(|n| n.parse::<usize>().ok());
                match (it.next(), it.next()) {
                    (Some(from), Some(to)) if from >= 1 && from <= self.tabs.len() => {
                        let number = self.tabs[from - 1].number;
                        self.reorder(number, to.saturating_sub(1), cx);
                        "ok".into()
                    }
                    _ => "move-session <from> <to>".into(),
                }
            }
            "tab" => {
                let n = arg.parse::<usize>().unwrap_or(1);
                self.activate(n.saturating_sub(1), window, cx);
                "ok".into()
            }
            "close-tab" => {
                let ix = self.active;
                self.close(ix, window, cx);
                "ok".into()
            }
            "rename-tab" => {
                let key = self.key(self.active);
                let _ = cx.global::<panels::Names>().0.borrow_mut().set(&key, arg);
                cx.notify();
                "ok".into()
            }
            "tabs" => (0..self.tabs.len())
                .map(|i| format!("{}{}:{}", if i == self.active { "*" } else { "" }, i + 1, self.name(i, cx)))
                .collect::<Vec<_>>()
                .join(" | "),
            _ if self.tabs.is_empty() => return Some("no session yet (the browser is open)".into()),
            _ => return None,
        })
    }

    // ----- rendering -----

    /// A session's dot: amber needs permission, blue working, white unseen, none when idle.
    fn dot(&self, ix: usize, cx: &App) -> Option<(Hsla, &'static str)> {
        let theme = cx.theme();
        let s = self.tabs[ix].ws.read(cx).session.read(cx);
        if s.chat.needs_input() {
            Some((theme.warning, "Needs permission"))
        } else if s.asking.is_some() {
            Some((theme.warning, "Asking something in the terminal"))
        } else if s.chat.busy || s.waiting_for.is_some() {
            Some((theme.blue, "Working"))
        } else if s.unseen && ix != self.active {
            Some((theme.foreground, "Something new"))
        } else {
            None
        }
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, hover, ring, bg, sel) =
            (theme.foreground, theme.muted_foreground, theme.border, theme.secondary_hover, theme.ring, theme.sidebar, theme.secondary);
        let mut col = div().id("session-list").flex_1().min_h_0().flex().flex_col().overflow_y_scroll();
        for ix in 0..self.tabs.len() {
            let active = ix == self.active && self.browser.is_none();
            let number = self.tabs[ix].number;
            let w = self.tabs[ix].ws.read(cx);
            let s = w.session.read(cx);
            let folder = w.root().file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let place = match w.branch() {
                Some(b) => format!("{folder} · {b}"),
                None => folder,
            };
            let place = if w.host().is_remote() { format!("{}: {place}", w.host().label()) } else { place };
            let worktree = w.worktree.clone();
            let activity = s.chat.last_activity(80);
            let cost = s.spend(cx).map(|sp| sp.short());
            let short_id = s.chat.session_id.clone();
            let dot = self.dot(ix, cx);
            let key = self.key(ix);
            let auto = self.list_name(ix, cx);
            let current = self.name(ix, cx);
            let title = panels::title_element(&key, auto.clone(), None, None, cx);
            let weak = cx.entity().downgrade();
            let (k1, a1, c1) = (key.clone(), auto.clone(), current.clone());
            let drag = SessionDrag { number, name: current.clone().into() };
            col = col.child(
                div()
                    .id(("session-row", number))
                    .relative()
                    .px_2()
                    .py(px(6.))
                    .flex()
                    .gap_2()
                    .border_b_1()
                    .border_color(border.opacity(0.5))
                    .cursor_pointer()
                    .when(active, |d| d.bg(sel))
                    .when(!active, |d| d.hover(|d| d.bg(hover)))
                    // The shown session: an accent bar along its left edge.
                    .when(active, |d| d.child(div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(ring)))
                    .child(
                        div()
                            .pt(px(5.))
                            .flex_none()
                            .w(px(10.))
                            .flex()
                            .justify_center()
                            .child(match dot {
                                Some((c, _)) => div().size(px(8.)).rounded_full().bg(c),
                                None => div().size(px(8.)).rounded_full().border_1().border_color(muted.opacity(0.6)),
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(div().flex_1().min_w_0().truncate().text_sm().text_color(fg).child(title))
                                    .children(cost.map(|c| div().flex_none().text_size(px(10.5)).text_color(muted).child(c))),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_size(px(11.))
                                    .text_color(muted)
                                    .child(div().min_w_0().truncate().child(place))
                                    .children(short_id.map(|id| panels::short_id(id, ("session-id", number), cx)))
                                    .children(worktree.map(|_| {
                                        div().flex_none().px_1().rounded_sm().bg(border).text_size(px(10.)).child("worktree")
                                    })),
                            )
                            .children(activity.map(|a| div().text_size(px(11.)).text_color(muted.opacity(0.8)).truncate().child(a))),
                    )
                    .on_click(cx.listener(move |s, ev: &ClickEvent, window, cx| {
                        if ev.click_count() >= 2 {
                            start_tab_rename(k1.clone(), a1.clone(), c1.clone(), window, cx);
                        } else if let Some(ix) = s.tabs.iter().position(|t| t.number == number) {
                            s.activate(ix, window, cx);
                        }
                    }))
                    .on_drag(drag, |d: &SessionDrag, _, _, cx| {
                        let name = d.name.clone();
                        cx.new(|_| DragLabel(name))
                    })
                    .drag_over::<SessionDrag>(move |d, _, _, _| d.border_t_2().border_color(ring))
                    .on_drop(cx.listener(move |s, d: &SessionDrag, _, cx| {
                        let to = s.tabs.iter().position(|t| t.number == number).unwrap_or(0);
                        s.reorder(d.number, to, cx)
                    }))
                    .context_menu(move |menu, _, _| {
                        let w1 = weak.clone();
                        let (k, a, c, k2) = (key.clone(), auto.clone(), current.clone(), key.clone());
                        menu.item(PopupMenuItem::new("Rename").on_click(move |_, window, cx| {
                            start_tab_rename(k.clone(), a.clone(), c.clone(), window, cx)
                        }))
                        .item(PopupMenuItem::new("Reset name").on_click(move |_, window, cx| panels::reset_tab_name(&k2, window, cx)))
                        .separator()
                        .item(PopupMenuItem::new("Close session").on_click(move |_, window, cx| {
                            let _ = w1.update(cx, |s, cx| {
                                if let Some(ix) = s.tabs.iter().position(|t| t.number == number) {
                                    s.close(ix, window, cx)
                                }
                            });
                        }))
                    }),
            );
        }
        // Room below the last row: dropping there moves a session to the end.
        let end = self.tabs.len();
        col = col.child(
            div()
                .id("session-list-end")
                .flex_1()
                .min_h(px(24.))
                .drag_over::<SessionDrag>(move |d, _, _, _| d.border_t_2().border_color(ring))
                .on_drop(cx.listener(move |s, d: &SessionDrag, _, cx| s.reorder(d.number, end, cx))),
        );
        div()
            .w(px(LIST_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(bg)
            .border_r_1()
            .border_color(border)
            .child(
                div()
                    .id("session-new")
                    .h(px(30.))
                    .flex_none()
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .text_color(muted)
                    .border_b_1()
                    .border_color(border)
                    .cursor_pointer()
                    .hover(|d| d.bg(hover).text_color(fg))
                    .child(Icon::new(IconName::Plus).small())
                    .child("New session")
                    .child(div().flex_1())
                    .child(div().text_xs().text_color(muted.opacity(0.7)).child("Cmd+N"))
                    .on_click(cx.listener(|s, _, window, cx| s.new_session(window, cx))),
            )
            .child(col)
            .into_any_element()
    }

    /// The folded list: a dot and initials per session, with the name in a tooltip.
    fn render_rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, hover, ring, bg, sel) =
            (theme.foreground, theme.muted_foreground, theme.border, theme.secondary_hover, theme.ring, theme.sidebar, theme.secondary);
        let mut col = div()
            .w(px(RAIL_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .py_1()
            .bg(bg)
            .border_r_1()
            .border_color(border)
            .child(
                div()
                    .id("rail-new")
                    .size(px(30.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .hover(|d| d.bg(hover))
                    .child(Icon::new(IconName::Plus).small().text_color(muted))
                    .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("New session (Cmd+N)").build(w, cx))
                    .on_click(cx.listener(|s, _, window, cx| s.new_session(window, cx))),
            );
        for ix in 0..self.tabs.len() {
            let active = ix == self.active && self.browser.is_none();
            let number = self.tabs[ix].number;
            let name = self.name(ix, cx);
            let activity = self.tabs[ix].ws.read(cx).session.read(cx).chat.last_activity(80);
            let dot = self.dot(ix, cx);
            let tip = match (&dot, activity) {
                (Some((_, state)), Some(a)) => format!("{name}\n{state}: {a}"),
                (Some((_, state)), None) => format!("{name}\n{state}"),
                (None, Some(a)) => format!("{name}\n{a}"),
                (None, None) => name.clone(),
            };
            col = col.child(
                div()
                    .id(("rail-session", number))
                    .relative()
                    .size(px(32.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if active { fg } else { muted })
                    .when(active, |d| d.bg(sel).border_1().border_color(ring))
                    .when(!active, |d| d.hover(|d| d.bg(hover)))
                    .child(initials(&name))
                    .children(dot.map(|(c, _)| {
                        div().absolute().top(px(2.)).right(px(2.)).size(px(8.)).rounded_full().border_1().border_color(bg).bg(c)
                    }))
                    .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
                    .on_click(cx.listener(move |s, _, window, cx| {
                        if let Some(ix) = s.tabs.iter().position(|t| t.number == number) {
                            s.activate(ix, window, cx);
                        }
                    })),
            );
        }
        col.into_any_element()
    }
}

/// Two letters for the rail: the first letters of the first two words.
fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let letters: String = match words.as_slice() {
        [] => "?".into(),
        [one] => one.chars().take(2).collect(),
        [a, b, ..] => a.chars().take(1).chain(b.chars().take(1)).collect(),
    };
    letters.to_uppercase()
}

/// The folder the browser starts in when nothing says otherwise: the last folder, else home.
/// Not checked here (that could wait on a sleeping disk); a folder that is gone lists empty.
pub fn default_folder(cx: &App) -> PathBuf {
    cx.global::<AppSettings>()
        .0
        .borrow()
        .recent
        .first()
        .cloned()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn folder_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string())
}

/// The repository a folder belongs to: its top folder and the branch checked out there.
pub fn repo_of(folder: &Path) -> Option<(PathBuf, Option<String>)> {
    let out = std::process::Command::new("git").arg("-C").arg(folder).args(["rev-parse", "--show-toplevel"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let top = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let branch = keleion_core::host::LocalHost.git_branch(&top);
    Some((top, branch))
}

/// A free name for a worktree of the repository at `top`: `<repo>-2`, `<repo>-3`, ... (a
/// folder next to the repository and a `keleion/<name>` branch, neither existing yet).
pub fn suggest_worktree_name(top: &Path) -> String {
    let name = folder_name(top);
    let parent = top.parent().unwrap_or(top);
    (2..).map(|n| format!("{name}-{n}")).find(|c| !parent.join(c).exists() && !branch_exists(top, &format!("keleion/{c}"))).unwrap_or(name)
}

fn branch_exists(top: &Path, branch: &str) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(top)
        .args(["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// `git worktree add -b keleion/<name> <parent of top>/<name>`: the folder and branch the
/// person saw and named. Refuses a name already taken.
fn make_worktree(top: &Path, name: &str) -> Result<(PathBuf, String), String> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return Err(format!("\"{name}\" is not a usable folder name"));
    }
    let parent = top.parent().ok_or("the repository has no parent folder")?;
    let path = parent.join(name);
    let branch = format!("keleion/{name}");
    if path.exists() {
        return Err(format!("{} exists already", path.display()));
    }
    if branch_exists(top, &branch) {
        return Err(format!("the branch {branch} exists already"));
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(top)
        .args(["worktree", "add", "-b", &branch])
        .arg(&path)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok((path, branch))
}

impl Focusable for Shell {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::first_frame(window);
        let theme = cx.theme();
        let (bg, fg, hover, muted) = (theme.background, theme.foreground, theme.secondary_hover, theme.muted_foreground);
        let button = |id: &'static str, icon: IconName, tip: &'static str| {
            div()
                .id(id)
                .size(px(22.))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded_sm()
                .hover(|d| d.bg(hover))
                .child(Icon::new(icon).small().text_color(muted))
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip).build(window, cx))
        };
        let to_ws = |f: fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
            cx.listener(move |s: &mut Shell, _: &ClickEvent, window: &mut Window, cx: &mut Context<Shell>| {
                if let Some(ws) = s.active_workspace() {
                    ws.update(cx, |w, cx| f(w, window, cx));
                }
            })
        };
        let title = if self.browser.is_some() || self.tabs.is_empty() {
            "Open a session".to_string()
        } else {
            self.name(self.active, cx)
        };
        let has_session = !self.tabs.is_empty() && self.browser.is_none();
        let top = TitleBar::new().h(px(34.)).pl(px(76.)).child(
            div()
                .size_full()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    button("t-sessions", IconName::PanelLeft, "Session list (Cmd+Shift+B)")
                        .on_click(cx.listener(|s, _, _, cx| s.toggle_folded(cx))),
                )
                .child(div().flex_1().min_w_0().truncate().text_xs().text_color(muted).px_2().child(title))
                .when(has_session, |d| {
                    d.child(
                        div()
                            .h_full()
                            .flex()
                            .items_center()
                            .gap_1()
                            .pr_1()
                            .child(
                                button("t-sidebar", IconName::PanelLeft, "Tool panels (Cmd+B)")
                                    .on_click(to_ws(|w, win, cx| w.toggle_sidebar_pub(win, cx))),
                            )
                            .child(
                                button("t-split", IconName::PanelRight, "Split the shown file or terminal (Cmd+\\)")
                                    .on_click(to_ws(|w, win, cx| w.split_right(win, cx))),
                            )
                            .child(button("t-reset", IconName::RotateCw, "Reset layout").on_click(to_ws(|w, win, cx| w.reset_layout(win, cx)))),
                    )
                }),
        );
        let main = match &self.browser {
            Some(b) => b.clone().into_any_element(),
            None => self.active_workspace().map(|w| w.into_any_element()).unwrap_or_else(|| div().into_any_element()),
        };
        let list = if self.tabs.is_empty() {
            None
        } else if self.folded {
            Some(self.render_rail(cx))
        } else {
            Some(self.render_list(cx))
        };
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(bg)
            .text_color(fg)
            .track_focus(&self.focus)
            .on_action(cx.listener(|s, _: &NewSession, w, cx| s.new_session(w, cx)))
            .on_action(cx.listener(|s, _: &ToggleSessions, _, cx| s.toggle_folded(cx)))
            .on_action(cx.listener(|s, _: &CloseSession, w, cx| {
                let ix = s.active;
                s.close(ix, w, cx)
            }))
            .on_action(cx.listener(|s, _: &NextSession, w, cx| {
                let ix = (s.active + 1) % s.tabs.len().max(1);
                s.activate(ix, w, cx)
            }))
            .on_action(cx.listener(|s, _: &PrevSession, w, cx| {
                let n = s.tabs.len().max(1);
                let ix = (s.active + n - 1) % n;
                s.activate(ix, w, cx)
            }))
            .on_action(cx.listener(|s, _: &OpenFolder, w, cx| s.new_session(w, cx)))
            .on_action(cx.listener(|s, _: &RenameSession, w, cx| {
                if s.tabs.is_empty() {
                    return;
                }
                let ix = s.active;
                let (key, auto, current) = (s.key(ix), s.list_name(ix, cx), s.name(ix, cx));
                start_tab_rename(key, auto, current, w, cx);
            }))
            .child(top)
            .child(div().flex_1().min_h_0().flex().children(list).child(div().flex_1().min_w_0().h_full().child(main)))
    }
}

/// Keeps one open from turning into several: the shown browser opens at most one session
/// (a double or triple click sends one event per click), and a conversation still loading is
/// not loaded again.
#[derive(Default)]
struct OpenGuard {
    chosen: bool,
    loading: std::collections::HashSet<String>,
}

impl OpenGuard {
    /// A browser was shown: it may open one session.
    fn browser_shown(&mut self) {
        self.chosen = false;
    }

    /// The browser asks to open something: true the first time only.
    fn choose(&mut self) -> bool {
        !std::mem::replace(&mut self.chosen, true)
    }

    /// Start loading `key`: false while it is loading already.
    fn start(&mut self, key: &str) -> bool {
        self.loading.insert(key.to_string())
    }

    fn finish(&mut self, key: &str) {
        self.loading.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::OpenGuard;

    #[::core::prelude::v1::test]
    fn a_triple_click_opens_one_session() {
        let mut g = OpenGuard::default();
        g.browser_shown();
        assert!(g.choose());
        assert!(!g.choose());
        assert!(!g.choose());
        // The next browser may open again.
        g.browser_shown();
        assert!(g.choose());
    }

    #[::core::prelude::v1::test]
    fn a_conversation_loading_is_not_loaded_twice() {
        let mut g = OpenGuard::default();
        assert!(g.start("abc"));
        assert!(!g.start("abc"));
        assert!(g.start("fork:abc"));
        g.finish("abc");
        assert!(g.start("abc"));
    }
}
