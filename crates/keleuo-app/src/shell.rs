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

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, TitleBar, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleuo_core::agent::{LiveReason, LiveSession};
use keleuo_core::agent::Backend;
use keleuo_core::host::{Host, LocalHost};

use crate::AppSettings;
use keleuo_core::settings::SavedSession;
use crate::actions::*;
use crate::agent_session::{AgentSession, Fill, SessionEvent, Spend};
use crate::attention::{self, Action, Need};
use crate::browser::{Browser, BrowserEvent, OpenSession};
use crate::panels::{self, start_tab_rename};
use crate::workspace::{Workspace, WorkspaceEvent};

const LIST_WIDTH: f32 = 250.;
const RAIL_WIDTH: f32 = 44.;
/// What a session that was working when Keleuo quit is told when it starts again.
const CONTINUE_MESSAGE: &str = "Keleuo was restarted while you were working. Continue where you left off.";
/// The rail's layout, shared with the list that peeks out over it, so the session under the
/// pointer stays under it when the list comes out: the "+" row, then a row per session.
const RAIL_NEW: f32 = 30.;
const RAIL_ROW: f32 = 32.;
const RAIL_GAP: f32 = 4.;
/// How wide the list that peeks out over the rail is.
const PEEK_WIDTH: f32 = 260.;
/// How long the pointer rests on the rail before the whole list peeks out.
const PEEK_DELAY_MS: u64 = 250;

/// No notifications, Dock bounces or badge: in tests, and in the background (test) mode.
fn silent() -> bool {
    cfg!(test) || crate::background()
}

struct Tab {
    number: usize,
    ws: Entity<Workspace>,
    /// What its row shows, kept up to date as the session changes (see [`Row`]).
    row: Row,
}

/// What a session's row in the list (and the rail) shows, kept on the tab. The list draws from
/// it, not from the sessions: GPUI draws the window again whenever something read while drawing
/// changes, so a list reading every session redrew the whole window on each streamed word and
/// each line of terminal output in any session, shown or not. Now a session's change redraws
/// the window only when its row comes out different.
#[derive(Clone, Default, PartialEq)]
struct Row {
    dot: Option<Dot>,
    unseen: bool,
    /// Its name from the conversation (the list's own, before any rename).
    name: String,
    root: PathBuf,
    remote: bool,
    folder: String,
    /// Folder, branch and machine.
    place: String,
    worktree: bool,
    activity: Option<String>,
    /// The last turn's summary, once the turn is over: status, whether it needs the person,
    /// headline.
    summary: Option<(keleuo_core::summary::Status, bool, String)>,
    per_use: Option<bool>,
    cost: Option<Spend>,
    context: Option<Spend>,
    short_id: Option<String>,
}

/// A session's state on its dot.
#[derive(Clone, Copy, PartialEq)]
enum Dot {
    Permission,
    Asking,
    Working,
    Agents,
}

impl Row {
    fn of(w: &Workspace, cx: &App) -> Self {
        let s = w.session.read(cx);
        let chat = &s.chat;
        let needs_input = chat.needs_input();
        let dot = if needs_input {
            Some(Dot::Permission)
        } else if s.asking.is_some() {
            Some(Dot::Asking)
        } else if chat.busy || s.waiting_for.is_some() {
            Some(Dot::Working)
        } else if chat.running_agents().any(|a| a.background) {
            Some(Dot::Agents)
        } else {
            None
        };
        let folder = w.root().file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let place = match w.branch() {
            Some(b) => format!("{folder} · {b}"),
            None => folder.clone(),
        };
        let place = if w.host().is_remote() { format!("{}: {place}", w.host().label()) } else { place };
        Row {
            dot,
            unseen: s.unseen,
            name: w.list_name(cx),
            root: w.root().to_path_buf(),
            remote: w.host().is_remote(),
            folder,
            place,
            worktree: w.worktree.is_some(),
            activity: chat.last_activity(80),
            summary: (!chat.busy && !needs_input)
                .then(|| chat.last_summary().map(|sm| (sm.status, !sm.needs_you.is_empty(), sm.headline.clone())))
                .flatten(),
            per_use: s.per_use,
            cost: s.spend_as(true),
            context: s.spend_as(false),
            short_id: chat.session_id.clone(),
        }
    }

    /// Cost or context, as the setting says (read when drawn, so a change of setting shows).
    fn spend(&self, cx: &App) -> Option<&Spend> {
        if crate::agent_session::shows_cost(self.per_use, cx) { self.cost.as_ref() } else { self.context.as_ref() }
    }
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
    /// Settings, shown in place of the session while open.
    settings: Option<(Entity<crate::settings_window::SettingsPage>, Subscription)>,
    folded: bool,
    /// The folded list is shown in full over the content (the rail was hovered).
    peek: bool,
    /// The pointer is over the rail (the list peeks out after a moment).
    rail_hover: bool,
    /// A press on the rail: no list peeks out until the pointer leaves it (the click goes to
    /// the session pressed).
    peek_held: bool,
    /// The folder being renamed, asked for its new name.
    folder_rename: Option<FolderRename>,
    /// Sessions from the last run are being opened again: the saved list is not touched.
    restoring: bool,
    /// The open sessions as last saved (written again only when they change).
    saved: Option<(Vec<SavedSession>, Option<usize>)>,
    focus: FocusHandle,
    subs: HashMap<EntityId, Vec<Subscription>>,
    _browser_sub: Option<Subscription>,
    /// What the shown browser opened, if anything: a double or triple click (or Enter held)
    /// must open one session, not one per click. Also guards resumes still loading.
    opening: OpenGuard,
    /// The last `earlier` listing (control channel).
    earlier: Vec<(keleuo_core::agent::SessionInfo, Option<u32>)>,
    /// What each session waits on the person for, to tell them once (see `attention`).
    attention: attention::Tracker,
    /// The count on the Dock icon.
    badge: usize,
    _attention: Vec<Subscription>,
}

impl Shell {
    pub fn new(start: Start, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let folded = cx.global::<AppSettings>().0.borrow().sessions_folded;
        // The window's close button: with something to lose, ask (as quitting does) and keep
        // the window until the answer.
        let weak = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            let Some(shell) = weak.upgrade() else { return true };
            if shell.read(cx).nothing_to_lose(cx) {
                return true;
            }
            shell.update(cx, |s, cx| s.confirm_quit(window, cx));
            false
        });
        let mut this = Self {
            tabs: Vec::new(),
            active: 0,
            next: 1,
            browser: None,
            settings: None,
            folded,
            peek: false,
            peek_held: false,
            rail_hover: false,
            folder_rename: None,
            restoring: false,
            saved: None,
            focus: cx.focus_handle(),
            subs: HashMap::new(),
            _browser_sub: None,
            opening: OpenGuard::default(),
            earlier: Vec::new(),
            attention: attention::Tracker::default(),
            badge: 0,
            _attention: vec![
                // Looking at the window again, or away; a notification setting changed.
                cx.observe_window_activation(window, |s, window, cx| s.attend(window, cx)),
                cx.observe_global_in::<AppSettings>(window, |s, window, cx| s.attend(window, cx)),
            ],
        };
        let (saved, shown) = {
            let settings = cx.global::<AppSettings>().0.borrow();
            // Settings: not reopening them starts afresh.
            let saved = if settings.prefs.reopen_sessions { settings.open_sessions.clone() } else { Vec::new() };
            (saved, settings.shown_session)
        };
        if saved.is_empty() {
            this.handle_start(start, window, cx);
        } else {
            this.restore(saved, shown, start, window, cx);
        }
        this
    }

    /// What the window was asked to show: a new session in a folder, or the browser at one
    /// (the session open there, if there is one).
    fn handle_start(&mut self, start: Start, window: &mut Window, cx: &mut Context<Self>) {
        match start {
            Start::Session(host, root) => {
                let backend = crate::backends::backend(None, cx);
                self.new_session_in(host, root, backend, window, cx);
            }
            Start::Browse(host, dir, note) => {
                // The shown session if it is in that folder, else the first one there.
                let here = |ix: &usize| self.tabs[*ix].ws.read(cx).root() == dir;
                let open_here = (!host.is_remote() && note.is_none())
                    .then(|| Some(self.active).filter(|ix| *ix < self.tabs.len() && here(ix)).or_else(|| (0..self.tabs.len()).find(here)))
                    .flatten();
                match open_here {
                    Some(ix) => self.activate(ix, window, cx),
                    None => self.open_browser(host, dir, note, window, cx),
                }
            }
        }
    }

    // ----- sessions kept between runs -----

    /// Open again the sessions of the last run, in their order and with their numbers (so their
    /// tab names stay theirs), each dormant: its conversation shows at once, its agent starts
    /// when it is needed. A folder or conversation that is gone is left out. Then `start`.
    fn restore(&mut self, saved: Vec<SavedSession>, shown: Option<usize>, start: Start, window: &mut Window, cx: &mut Context<Self>) {
        self.restoring = true;
        let backends = crate::backends::all(cx);
        let fallback = crate::backends::backend(None, cx);
        cx.spawn_in(window, async move |this, cx| {
            let loaded = cx
                .background_spawn(async move {
                    let host = LocalHost;
                    saved
                        .into_iter()
                        .filter(|s| s.folder.is_dir())
                        .filter_map(|s| {
                            let backend = backends.iter().find(|b| b.id() == s.backend).cloned().unwrap_or_else(|| fallback.clone());
                            let chat = match &s.id {
                                Some(id) => Some(backend.load_session(&host, &s.folder, id).ok()?),
                                None => None,
                            };
                            Some((s, backend, chat))
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update_in(cx, |shell, window, cx| {
                let host: Arc<dyn Host> = Arc::new(LocalHost);
                let mut last = shell.next;
                for (s, backend, chat) in loaded {
                    let mut session = match chat {
                        Some(chat) => AgentSession::resumed(host.clone(), backend, s.folder.clone(), chat, false),
                        None => AgentSession::new(host.clone(), backend, s.folder.clone()),
                    };
                    session.dormant = true;
                    session.writes_anywhere = s.writes_anywhere;
                    session.writable = s.writable.clone();
                    shell.next = s.number.max(1);
                    shell.add_session(host.clone(), s.folder, session, None, None, window, cx);
                    last = last.max(s.number + 1);
                }
                shell.next = last.max(shell.next);
                shell.restoring = false;
                if let Some(ix) = shown.and_then(|n| shell.tabs.iter().position(|t| t.number == n)) {
                    shell.activate(ix, window, cx);
                }
                shell.handle_start(start, window, cx);
                shell.save_open(cx);
                shell.continue_working(cx);
            });
        })
        .detach();
    }

    /// Sessions that were working when Keleuo quit, and the person asked to carry on: each is
    /// told to (its agent starts for it, and takes the message once it is up).
    fn continue_working(&mut self, cx: &mut Context<Self>) {
        let settings = cx.global::<AppSettings>().0.clone();
        let ids = std::mem::take(&mut settings.borrow_mut().continue_on_start);
        if ids.is_empty() {
            return;
        }
        let _ = settings.borrow().save();
        for tab in &self.tabs {
            let session = tab.ws.read(cx).session.clone();
            if session.read(cx).chat.session_id.as_ref().is_some_and(|id| ids.contains(id)) {
                session.update(cx, |s, cx| s.send(CONTINUE_MESSAGE, cx));
            }
        }
    }

    /// Quitting: when sessions are working, ask whether they should carry on when Keleuo starts
    /// again (or not quit at all); otherwise quit.
    pub fn confirm_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let unsaved: Vec<_> = self.tabs.iter().flat_map(|t| t.ws.read(cx).unsaved_docs(cx)).collect();
        let working: Vec<(String, String)> = (0..self.tabs.len())
            .filter_map(|ix| {
                let s = self.tabs[ix].ws.read(cx).session.read(cx);
                let id = s.chat.session_id.clone().filter(|_| s.chat.busy)?;
                Some((id, self.name(ix, cx)))
            })
            .collect();
        let commands: usize = self.tabs.iter().map(|t| t.ws.read(cx).busy_terminals(cx)).sum();
        if unsaved.is_empty() && working.is_empty() && commands == 0 {
            crate::quit_now(cx);
            return;
        }
        let what = crate::closing::busy_summary(working.len(), commands);
        let names: Vec<String> = working.iter().map(|(_, n)| n.clone()).collect();
        let ids: Vec<String> = working.into_iter().map(|(id, _)| id).collect();
        let handle = window.window_handle();
        cx.spawn(async move |_, cx| {
            // Unsaved files first, as any app asks; then what is still running.
            if !crate::closing::save_or_drop(unsaved, handle, cx).await {
                return;
            }
            let mut carry_on = Vec::new();
            if !names.is_empty() || commands > 0 {
                let (message, detail, answers) = if names.is_empty() {
                    // Only commands: they stop with Keleuo, and there is nothing to carry on.
                    (format!("{what}. Quit anyway?"), "The command stops when Keleuo quits.".to_string(), vec![
                        PromptButton::Ok("Quit".into()),
                        PromptButton::Cancel("Cancel".into()),
                    ])
                } else {
                    let (them, they, carry, wait) =
                        if names.len() > 1 { ("them", "they", "carry", "wait") } else { ("it", "it", "carries", "waits") };
                    (
                        format!("{what}. Continue {them} when Keleuo starts again?"),
                        format!(
                            "{}\n\nYes: {they} {carry} on when Keleuo starts again.\nNo: quit; {they} {wait} for your next message.",
                            names.join("\n"),
                        ),
                        vec![PromptButton::Ok("Yes".into()), PromptButton::Other("No".into()), PromptButton::Cancel("Cancel".into())],
                    )
                };
                let cancel = answers.len() - 1;
                let continues = !names.is_empty();
                match crate::closing::ask(handle, PromptLevel::Info, message, detail, answers, cx).await {
                    Some(a) if a == cancel => return,
                    None => return,
                    Some(0) if continues => carry_on = ids,
                    Some(_) => {}
                }
            }
            cx.update(|cx| {
                let settings = cx.global::<AppSettings>().0.clone();
                settings.borrow_mut().continue_on_start = carry_on;
                let _ = settings.borrow().save();
                crate::quit_now(cx);
            });
        })
        .detach();
    }

    /// Nothing open would be lost by closing the window: no unsaved file, no session working,
    /// no terminal running a command.
    fn nothing_to_lose(&self, cx: &App) -> bool {
        self.tabs.iter().all(|t| {
            let w = t.ws.read(cx);
            w.unsaved_docs(cx).is_empty() && !w.session.read(cx).chat.busy && w.busy_terminals(cx) == 0
        })
    }

    /// Close the session `ix`, asking first when that loses something: its unsaved files, its
    /// turn in progress, its terminals' commands.
    pub fn request_close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else { return };
        let number = tab.number;
        let w = tab.ws.read(cx);
        let unsaved = w.unsaved_docs(cx);
        let working = w.session.read(cx).chat.busy;
        let commands = w.busy_terminals(cx);
        if unsaved.is_empty() && !working && commands == 0 {
            self.close(ix, window, cx);
            return;
        }
        let name = self.name(ix, cx);
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            if !crate::closing::save_or_drop(unsaved, handle, cx).await {
                return;
            }
            if working || commands > 0 {
                let message = if working {
                    format!("\u{201c}{name}\u{201d} is working. Close it anyway?")
                } else {
                    format!("{}. Close \u{201c}{name}\u{201d} anyway?", crate::closing::busy_summary(0, commands))
                };
                let detail = if working { "Its agent stops in the middle of its turn." } else { "The command stops with the session." };
                let answers = vec![PromptButton::Ok("Close".into()), PromptButton::Cancel("Cancel".into())];
                if crate::closing::ask(handle, PromptLevel::Warning, message, detail.into(), answers, cx).await != Some(0) {
                    return;
                }
            }
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |s, cx| {
                    if let Some(ix) = s.tabs.iter().position(|t| t.number == number) {
                        s.close(ix, window, cx);
                    }
                });
            });
        })
        .detach();
    }

    /// Keep the list of open sessions (and the shown one) for the next run, when it changed.
    fn save_open(&mut self, cx: &mut Context<Self>) {
        if self.restoring {
            return;
        }
        let list: Vec<SavedSession> = self
            .tabs
            .iter()
            .filter_map(|t| {
                let w = t.ws.read(cx);
                if w.host().is_remote() {
                    return None;
                }
                let s = w.session.read(cx);
                Some(SavedSession {
                    number: t.number,
                    folder: w.root().to_path_buf(),
                    backend: s.backend.id().to_string(),
                    id: s.chat.session_id.clone(),
                    writes_anywhere: s.writes_anywhere,
                    writable: s.writable.clone(),
                })
            })
            .collect();
        let shown = self.tabs.get(self.active).map(|t| t.number);
        let now = (list, shown);
        if self.saved.as_ref() == Some(&now) {
            return;
        }
        let settings = cx.global::<AppSettings>().0.clone();
        {
            let mut s = settings.borrow_mut();
            s.open_sessions = now.0.clone();
            s.shown_session = now.1;
        }
        let _ = settings.borrow().save();
        self.saved = Some(now);
    }

    /// The sessions in this window, for messages between them (see `messaging`): each one's
    /// number, name, agent, folder and session. The name agents use is the one the person gave
    /// it, else its folder's ("kk-beta"; "kk-beta #2" for a second session there): an
    /// automatic name (the start of its first message) is neither short nor stable.
    pub fn directory(&self, cx: &App) -> Vec<crate::messaging::Peer> {
        let mut seen: HashMap<String, usize> = HashMap::new();
        self.tabs
            .iter()
            .map(|t| {
                let ws = t.ws.read(cx);
                let session = ws.session.clone();
                let given = cx.global::<panels::Names>().0.borrow().get(&panels::session_key(t.number)).map(str::to_string);
                let name = given.unwrap_or_else(|| {
                    let folder = folder_name(ws.root());
                    let n = seen.entry(folder.clone()).or_insert(0);
                    *n += 1;
                    if *n == 1 { folder } else { format!("{folder} #{n}") }
                });
                crate::messaging::Peer {
                    number: t.number,
                    name,
                    agent: session.read(cx).backend.name().to_string(),
                    folder: ws.root().to_path_buf(),
                    session,
                }
            })
            .collect()
    }

    /// Show Settings in place of the session (focused, so Escape closes it).
    pub fn show_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_settings_at("general", false, window, cx)
    }

    /// Show Settings on page `name` (a fresh page when it is not the open one's); `here`: the
    /// MCP servers page on the shown session's folder.
    pub fn show_settings_at(&mut self, name: &str, here: bool, window: &mut Window, cx: &mut Context<Self>) {
        if name != "general" {
            self.settings = None;
        }
        let page = match &self.settings {
            Some((page, _)) => page.clone(),
            None => {
                // Folders for the MCP servers page: the shown session's, then the recent ones.
                let mut folders: Vec<PathBuf> = self.active_workspace().map(|w| w.read(cx).root().to_path_buf()).into_iter().collect();
                for f in cx.global::<AppSettings>().0.borrow().recent.iter() {
                    if !folders.contains(f) {
                        folders.push(f.clone());
                    }
                }
                let folder = here.then(|| folders.first().cloned()).flatten();
                let name = name.to_string();
                let page = cx.new(|cx| crate::settings_window::SettingsPage::new(folders, cx).starting_at(&name, folder));
                let sub = cx.subscribe_in(&page, window, |s, _, _: &crate::settings_window::CloseSettings, window, cx| {
                    s.close_settings(window, cx)
                });
                self.settings = Some((page.clone(), sub));
                page
            }
        };
        window.focus(&page.focus_handle(cx), cx);
        cx.notify();
    }

    /// Back from Settings to the session (or New session) it covered.
    pub fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.take().is_some() {
            if let Some(w) = self.active_workspace() {
                w.update(cx, |w, cx| w.focus_main(window, cx));
            }
            cx.notify();
        }
    }

    pub fn active_workspace(&self) -> Option<Entity<Workspace>> {
        self.tabs.get(self.active).map(|t| t.ws.clone())
    }

    fn open_sessions(&self, cx: &App) -> Vec<OpenSession> {
        self.tabs
            .iter()
            .map(|t| {
                OpenSession { id: t.ws.read(cx).session.read(cx).chat.session_id.clone() }
            })
            .collect()
    }

    // ----- the browser -----

    /// Show the directory browser at `dir` (in place of the shown session, if any).
    pub fn open_browser(&mut self, host: Arc<dyn Host>, dir: PathBuf, note: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.settings = None;
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
            BrowserEvent::New { host, root, backend, worktree, name } => {
                let worktree = worktree.clone();
                if !self.opening.choose() {
                    return;
                }
                if let Some(folder) = worktree {
                    if let Err(e) = self.new_session_in_worktree(root.clone(), Some(folder), backend.clone(), window, cx) {
                        // Said beside Create, what was entered kept; the browser may be used again.
                        self.opening.browser_shown();
                        if let Some(b) = &self.browser {
                            b.update(cx, |b, cx| b.creation_failed(format!("Could not make the worktree: {e}"), cx));
                        }
                        return;
                    }
                } else {
                    self.new_session_in(host.clone(), root.clone(), backend.clone(), window, cx);
                }
                self.name_shown(name.as_deref().unwrap_or(""), window, cx);
            }
            BrowserEvent::Resume { host, root, backend, id, fork, wait_for, name } => {
                if !self.opening.choose() {
                    return;
                }
                self.resume(host.clone(), root.clone(), backend.clone(), id.clone(), *fork, *wait_for, false, name.clone(), window, cx)
            }
        }
    }

    fn find_session(&self, id: &str, cx: &App) -> Option<usize> {
        self.tabs.iter().position(|t| t.ws.read(cx).session.read(cx).chat.session_id.as_deref() == Some(id))
    }

    /// Continue an earlier session (read off the UI thread). One already open here just shows.
    /// `closed_here`: this window stopped it just now, so it is not checked for being open
    /// elsewhere (its file changed seconds ago, which would otherwise say it may be).
    #[allow(clippy::too_many_arguments)]
    fn resume(
        &mut self,
        host: Arc<dyn Host>,
        root: PathBuf,
        backend: Arc<dyn Backend>,
        id: String,
        fork: bool,
        wait_for: Option<u32>,
        closed_here: bool,
        name: Option<String>,
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
            let plain = !fork && wait_for.is_none() && !closed_here;
            let (live, chat) = cx
                .background_spawn(async move {
                    // Checked now, not when the list was drawn: never a second writer on a
                    // session open elsewhere.
                    let live = if plain { b.session_live(h.as_ref(), &r, &id, std::process::id()) } else { None };
                    let started = std::time::Instant::now();
                    let chat = if live.is_some() { None } else { Some(b.load_session(h.as_ref(), &r, &id)) };
                    if crate::perf::enabled() {
                        eprintln!("keleuo: session read in {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
                    }
                    (live, chat)
                })
                .await;
            let _ = this.update_in(cx, |s, window, cx| {
                s.opening.finish(&key);
                match (live, chat) {
                    (Some(live), _) => s.refused(live, window, cx),
                    (None, Some(chat)) => s.opened(chat, host, root, backend, fork, wait_for, name, window, cx),
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
        chat: std::io::Result<keleuo_core::conversation::Conversation>,
        host: Arc<dyn Host>,
        root: PathBuf,
        backend: Arc<dyn Backend>,
        fork: bool,
        wait_for: Option<u32>,
        name: Option<String>,
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
                    // A fork named as it was made.
                    if let Some(name) = name {
                        s.name_shown(&name, window, cx);
                    }
                }
                Err(e) => {
                    // Nothing opened: the browser may be used again.
                    s.opening.browser_shown();
                    window.push_notification(format!("Could not read the session: {e}"), cx)
                }
        }
    }

    // ----- renaming a folder -----

    /// Ask for a new name for `folder` (a session's folder on this machine).
    pub fn ask_rename_folder(&mut self, folder: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = folder_name(&folder);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        input.update(cx, |i, cx| {
            i.focus(window, cx);
            i.select_all(window, cx);
        });
        let sub = cx.subscribe_in(&input, window, |s, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { .. } => s.rename_folder(window, cx),
            InputEvent::Change => {
                if let Some(r) = s.folder_rename.as_mut() {
                    r.error = None;
                }
                cx.notify();
            }
            _ => {}
        });
        self.folder_rename = Some(FolderRename { folder, input, error: None, _sub: sub });
        cx.notify();
    }

    /// Rename the folder asked about: its sessions close, the folder and what the agents keep
    /// for it (sessions, notes) move to the new name, tab names and recent folders follow,
    /// and the same sessions open again there.
    fn rename_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.folder_rename.as_mut() else { return };
        let name = r.input.read(cx).value().trim().to_string();
        let old = r.folder.clone();
        let Some(parent) = old.parent().map(Path::to_path_buf) else { return };
        let new = parent.join(&name);
        let problem = if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            Some("A folder name, without slashes.".to_string())
        } else if new == old {
            None
        } else if new.exists() {
            Some(format!("{name} exists already in {}.", parent.display()))
        } else {
            None
        };
        if let Some(p) = problem {
            r.error = Some(p);
            cx.notify();
            return;
        }
        self.folder_rename = None;
        if new == old {
            cx.notify();
            return;
        }
        // The sessions working in it (or below it), closed now and opened again after.
        let mut reopen: Vec<Reopen> = Vec::new();
        for ix in (0..self.tabs.len()).rev() {
            let (host, root, backend, id) = {
                let w = self.tabs[ix].ws.read(cx);
                let s = w.session.read(cx);
                (w.host(), w.root().to_path_buf(), s.backend.clone(), s.chat.session_id.clone())
            };
            if host.is_remote() || !root.starts_with(&old) {
                continue;
            }
            let moved = new.join(root.strip_prefix(&old).unwrap_or(Path::new("")));
            let moved = if moved.as_os_str().to_string_lossy().ends_with('/') { new.clone() } else { moved };
            reopen.push((host, moved, backend, id));
            self.close(ix, window, cx);
        }
        reopen.reverse();
        let backends: Vec<Arc<dyn Backend>> = cx.global::<crate::backends::Backends>().0.all().to_vec();
        let (from, to) = (old.clone(), new.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    // The closed sessions' agents a moment to exit.
                    std::thread::sleep(std::time::Duration::from_millis(600));
                    std::fs::rename(&from, &to)?;
                    let mut kept = Vec::new();
                    for b in &backends {
                        if let Err(e) = b.folder_renamed(&LocalHost, &from, &to) {
                            kept.push(format!("{}: {e}", b.name()));
                        }
                    }
                    Ok::<_, std::io::Error>(kept)
                })
                .await;
            let _ = this.update_in(cx, |s, window, cx| {
                let renamed = result.is_ok();
                match &result {
                    Ok(kept) => {
                        let settings = cx.global::<AppSettings>().0.clone();
                        settings.borrow_mut().folder_renamed(&old, &new);
                        let _ = settings.borrow().save();
                        let _ = cx.global::<panels::Names>().0.borrow_mut().folder_renamed(&old, &new);
                        let note = if kept.is_empty() {
                            format!("Renamed {} to {}.", folder_name(&old), folder_name(&new))
                        } else {
                            format!("Renamed {} to {}, but some history stayed behind ({}).", folder_name(&old), folder_name(&new), kept.join("; "))
                        };
                        window.push_notification(note, cx);
                    }
                    Err(e) => window.push_notification(format!("Could not rename {}: {e}", folder_name(&old)), cx),
                }
                // Open the same sessions again: in the new place, or where they were.
                for (host, moved, backend, id) in reopen {
                    let root = if renamed { moved } else { old.join(moved.strip_prefix(&new).unwrap_or(Path::new(""))) };
                    match id {
                        // Closed by this window a moment ago: its file is fresh, not open elsewhere.
                        Some(id) => s.resume(host, root, backend, id, false, None, true, None, window, cx),
                        None => s.new_session_in(host, root, backend, window, cx),
                    }
                }
                if s.tabs.is_empty() {
                    let dir = if renamed { new.clone() } else { old.clone() };
                    s.open_browser(Arc::new(LocalHost), dir, None, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_folder_rename(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let r = self.folder_rename.as_ref()?;
        let theme = cx.theme();
        let (fg, muted, border, bg, red) = (theme.foreground, theme.muted_foreground, theme.border, theme.background, theme.red);
        let open_here = self.tabs.iter().filter(|t| t.ws.read(cx).root().starts_with(&r.folder)).count();
        let note = match open_here {
            0 => "Claude Code's sessions and notes for it move along.".to_string(),
            1 => "Its session closes, and opens again in the renamed folder. Claude Code's sessions and notes for it move along.".to_string(),
            n => format!("Its {n} sessions close, and open again in the renamed folder. Claude Code's sessions and notes for it move along."),
        };
        Some(
            div()
                .id("folder-rename")
                .absolute()
                .inset_0()
                .flex()
                // Only as tall as what it holds, near the top.
                .items_start()
                .justify_center()
                .pt(px(90.))
                .bg(gpui_kit::black().opacity(0.25))
                .occlude()
                // Escape cancels, wherever the focus is in it.
                .capture_key_down(cx.listener(|s, ev: &KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        s.folder_rename = None;
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
                .child(
                    div()
                        .w(px(440.))
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .rounded_lg()
                        .border_1()
                        .border_color(border)
                        .bg(bg)
                        .shadow_lg()
                        .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(fg).child("Rename folder"))
                        .child(div().text_xs().text_color(muted).child(r.folder.display().to_string()))
                        .child(Input::new(&r.input).small())
                        .children(r.error.clone().map(|e| div().text_xs().text_color(red).child(e)))
                        .child(div().text_xs().line_height(rems(1.4)).text_color(muted).child(note))
                        .child(
                            div()
                                .pt_1()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(Button::new("folder-rename-cancel").small().ghost().label("Cancel").on_click(cx.listener(|s, _, _, cx| {
                                    s.folder_rename = None;
                                    cx.notify();
                                })))
                                .child(Button::new("folder-rename-go").small().primary().label("Rename").on_click(cx.listener(|s, _, window, cx| s.rename_folder(window, cx)))),
                        ),
                )
                .into_any_element(),
        )
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
                WorkspaceEvent::AgentEdited(path) => shell.edited(ws.clone(), path.clone(), window, cx),
            }),
            // The row's name, dot, activity and cost follow the session and its workspace (drawn
            // again only when the row changed); its conversation id, once it has one, is kept
            // for the next run.
            cx.observe(&ws, |s, ws, cx| {
                if s.refresh_row(&ws, cx) {
                    cx.notify();
                }
            }),
            cx.observe_in(&session, window, {
                let ws = ws.downgrade();
                move |s, _, window, cx| {
                    s.save_open(cx);
                    s.attend(window, cx);
                    if let Some(ws) = ws.upgrade()
                        && s.refresh_row(&ws, cx)
                    {
                        cx.notify();
                    }
                }
            }),
            cx.subscribe_in(&session, window, |s, session, ev: &SessionEvent, window, cx| {
                if let SessionEvent::TurnEnded = ev {
                    s.turn_ended(session.clone(), window, cx);
                }
            }),
        ];
        let number = self.next;
        self.next += 1;
        self.subs.insert(ws.entity_id(), subs);
        let row = Row::of(ws.read(cx), cx);
        self.tabs.push(Tab { number, ws, row });
        self.activate(self.tabs.len() - 1, window, cx);
        self.save_open(cx);
    }

    pub fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        // Picking a session leaves Settings.
        self.settings = None;
        self.active = ix;
        // A session picked from the peeking list leaves the list out: it goes when the pointer
        // moves on over the content.
        self.browser = None;
        self._browser_sub = None;
        let ws = self.tabs[ix].ws.clone();
        ws.update(cx, |w, cx| {
            w.session.update(cx, |s, cx| {
                s.unseen = false;
                // Reopened from the last run with its Terminal showing: start it now.
                s.wake(cx);
            });
            w.focus_main(window, cx);
        });
        // Seen now: its row loses the mark.
        self.refresh_row(&ws, cx);
        self.save_open(cx);
        let title = format!("{} - Keleuo", self.name(ix, cx));
        window.set_window_title(&title);
        self.attend(window, cx);
        cx.notify();
    }

    pub fn close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(ix);
        tab.ws.update(cx, |w, cx| w.session.update(cx, |s, cx| s.stop(cx)));
        let key = tab.ws.read(cx).session.entity_id().as_u64();
        self.attention.forget(key);
        self.set_badge(cx);
        self.subs.remove(&tab.ws.entity_id());
        // Closed on purpose: not opened again next time.
        self.save_open(cx);
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
        self.save_open(cx);
        cx.notify();
    }

    fn key(&self, ix: usize) -> String {
        panels::session_key(self.tabs[ix].number)
    }

    fn list_name(&self, ix: usize, _cx: &App) -> String {
        self.tabs[ix].row.name.clone()
    }

    /// Bring the row of `ws`'s session up to date; true when it changed (the list is then
    /// drawn again).
    fn refresh_row(&mut self, ws: &Entity<Workspace>, cx: &App) -> bool {
        let Some(tab) = self.tabs.iter_mut().find(|t| &t.ws == ws) else { return false };
        let row = Row::of(ws.read(cx), cx);
        if tab.row == row {
            return false;
        }
        tab.row = row;
        true
    }

    fn name(&self, ix: usize, cx: &App) -> String {
        panels::tab_label(&self.key(ix), self.list_name(ix, cx), cx)
    }

    /// A session's right-click menu, the same from the list, the rail and the peeking list.
    fn session_menu(
        &self,
        ix: usize,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let number = self.tabs[ix].number;
        let (key, auto, current) = (self.key(ix), self.list_name(ix, cx), self.name(ix, cx));
        let row = &self.tabs[ix].row;
        let (remote, folder) = (row.remote, row.root.clone());
        let weak = cx.entity().downgrade();
        move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            let (w1, w3) = (weak.clone(), weak.clone());
            let (k, a, c, k2) = (key.clone(), auto.clone(), current.clone(), key.clone());
            let path = folder.display().to_string();
            menu.item(PopupMenuItem::new("Rename").on_click(move |_, window, cx| {
                start_tab_rename(k.clone(), a.clone(), c.clone(), window, cx);
                // Folded, the name is edited in the list peeking out over the rail.
                let _ = w3.update(cx, |s, cx| {
                    if s.folded {
                        s.peek = true;
                        cx.notify();
                    }
                });
            }))
            .item(PopupMenuItem::new("Reset name").on_click(move |_, window, cx| panels::reset_tab_name(&k2, window, cx)))
            .separator()
            .when(!remote, |m| {
                let (w2, f, f2) = (weak.clone(), folder.clone(), folder.clone());
                m.item(PopupMenuItem::new("Reveal in Finder").on_click(move |_, _, cx| cx.reveal_path(&f2)))
                    .item(PopupMenuItem::new("Rename folder…").on_click(move |_, window, cx| {
                        let _ = w2.update(cx, |s, cx| s.ask_rename_folder(f.clone(), window, cx));
                    }))
            })
            .item(PopupMenuItem::new("Copy folder path").on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(path.clone()))
            }))
            .separator()
            .item(PopupMenuItem::new("Close session").on_click(move |_, window, cx| {
                let _ = w1.update(cx, |s, cx| {
                    if let Some(ix) = s.tabs.iter().position(|t| t.number == number) {
                        s.request_close(ix, window, cx)
                    }
                });
            }))
        }
    }

    /// Give the shown session a name, as a rename does (empty: named after its first message).
    fn name_shown(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = (self.active < self.tabs.len()).then(|| self.key(self.active)) else { return };
        let _ = cx.global::<panels::Names>().0.borrow_mut().set(&key, name);
        window.set_window_title(&format!("{} - Keleuo", self.name(self.active, cx)));
        cx.notify();
    }

    fn toggle_folded(&mut self, cx: &mut Context<Self>) {
        self.folded = !self.folded;
        self.peek = false;
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
                // A session on the scripted test backend (needs KELEUO_TEST_BACKENDS=1).
                // It is registered last.
                let Some(b) = crate::backends::all(cx).into_iter().skip(1).last() else {
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
            "message-decide" => {
                // Tests: answer a message's card (`<id> allow|always|deny`), as its buttons do.
                let mut words = arg.split_whitespace();
                let id = words.next().and_then(|w| w.parse::<u64>().ok());
                let decision = match words.next() {
                    Some("allow") => Some(crate::messaging::Decision::Allow),
                    Some("always") => Some(crate::messaging::Decision::Always),
                    Some("deny") => Some(crate::messaging::Decision::Deny),
                    _ => None,
                };
                match (id, decision) {
                    (Some(id), Some(d)) => {
                        crate::messaging::decide(id, d, cx);
                        "ok".into()
                    }
                    _ => "message-decide <id> allow|always|deny".into(),
                }
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
            "browser-name" => {
                // Tests: type a name for the new session in the browser.
                match self.browser.clone() {
                    Some(b) => {
                        b.update(cx, |b, cx| b.test_name(arg, window, cx));
                        "ok".into()
                    }
                    None => "no browser".into(),
                }
            }
            "show-image" => {
                // Tests: open an image full size, as a click on one in the chat does.
                crate::image_view::show(PathBuf::from(arg), cx);
                "ok".into()
            }
            "installer" => return Some(crate::skill_installer::test_command(arg, cx)),
            "close-image" => {
                crate::image_view::close(cx);
                "ok".into()
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
                self.resume(host, root, backend, s.id, fork, wait, false, None, window, cx);
                "ok".into()
            }
            "peek" => {
                // Tests: the folded list peeks out (`peek off` puts it back).
                self.peek = arg != "off" && self.folded;
                cx.notify();
                format!("peek={}", self.peek)
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
            "about" => {
                crate::about::open_tab(arg, cx);
                "ok".into()
            }
            "settings" => {
                // Tests: `settings`, `settings off`, `settings <page>` (`mcp here`: on this
                // session's folder).
                match arg {
                    "off" => self.close_settings(window, cx),
                    "" => self.show_settings(window, cx),
                    _ => {
                        let mut w = arg.split_whitespace();
                        let page = w.next().unwrap_or("general").to_string();
                        self.show_settings_at(&page, w.next() == Some("here"), window, cx)
                    }
                }
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

    // ----- telling the person (see `attention`) -----

    /// The session `ix` is in front of the person: shown, in the focused window.
    fn looking_at(&self, ix: usize, window: &Window) -> bool {
        window.is_window_active() && ix == self.active && self.browser.is_none()
    }

    /// Tell the person about sessions that came to wait on them (a permission prompt, a
    /// question in the terminal) while they look elsewhere, and take back what they have seen.
    fn attend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if silent() {
            return;
        }
        let now = std::time::Instant::now();
        for ix in 0..self.tabs.len() {
            let session = self.tabs[ix].ws.read(cx).session.clone();
            let s = session.read(cx);
            let live = if let Some(p) = s.chat.pending_permissions().next() {
                Some(Need::Permission { tool: p.tool_name.clone(), summary: p.summary.clone() })
            } else {
                s.asking.map(|_| Need::Asking)
            };
            let looking = self.looking_at(ix, window);
            let action = self.attention.update(session.entity_id().as_u64(), live, looking, now);
            self.act(action, cx);
        }
        self.set_badge(cx);
    }

    /// A session's turn ended: once its reply is in (its last words may still be on their way
    /// from the session file), tell the person if they look elsewhere.
    fn turn_ended(&mut self, session: Entity<AgentSession>, window: &mut Window, cx: &mut Context<Self>) {
        if silent() {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(1500)).await;
            let _ = this.update_in(cx, |s, window, cx| {
                let Some(ix) = s.tabs.iter().position(|t| t.ws.read(cx).session == session) else { return };
                let chat = &session.read(cx).chat;
                // Working again, or waiting on a question (told about on its own).
                if chat.busy || chat.needs_input() || session.read(cx).asking.is_some() {
                    return;
                }
                let need = Need::Finished(chat.last_activity(100));
                let looking = s.looking_at(ix, window);
                let action = s.attention.update(session.entity_id().as_u64(), Some(need), looking, std::time::Instant::now());
                s.act(action, cx);
                s.set_badge(cx);
            });
        })
        .detach();
    }

    /// Carry out what the tracker decided, as the settings allow.
    fn act(&self, action: Action, cx: &App) {
        match action {
            Action::Announce(need) => {
                let notify = &cx.global::<AppSettings>().0.borrow().notify;
                if if need.is_question() { notify.waiting } else { notify.finished } {
                    attention::bounce();
                }
            }
            // Seen: nothing to take back while there are no banners.
            Action::Withdraw | Action::Nothing => {}
        }
    }

    /// The Dock icon's count: sessions waiting on the person, of the kinds turned on.
    fn set_badge(&mut self, cx: &App) {
        let count = {
            let notify = &cx.global::<AppSettings>().0.borrow().notify;
            self.attention.count(notify.waiting, notify.finished)
        };
        if count != self.badge && !silent() {
            self.badge = count;
            attention::set_badge(count);
        }
    }


    // ----- rendering -----

    /// A session's dot: amber needs permission, blue working, white unseen, none when idle.
    fn dot(&self, ix: usize, cx: &App) -> Option<(Hsla, &'static str)> {
        let theme = cx.theme();
        let row = &self.tabs[ix].row;
        match row.dot {
            Some(Dot::Permission) => Some((theme.warning, "Needs permission")),
            Some(Dot::Asking) => Some((theme.warning, "Asking something in the terminal")),
            Some(Dot::Working) => Some((theme.blue, "Working")),
            Some(Dot::Agents) => Some((theme.magenta, "Agents still working in the background")),
            None if row.unseen && ix != self.active => Some((theme.foreground, "Something new")),
            None => None,
        }
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, hover, ring, bg, sel) =
            (theme.foreground, theme.muted_foreground, theme.border, theme.secondary_hover, theme.ring, theme.sidebar, theme.secondary);
        let (yellow, red, blue) = (theme.yellow, theme.red, theme.blue);
        let mut col = div().id("session-list").flex_1().min_h_0().flex().flex_col().overflow_y_scroll();
        for ix in 0..self.tabs.len() {
            let active = ix == self.active && self.browser.is_none();
            let number = self.tabs[ix].number;
            let row = &self.tabs[ix].row;
            let place = row.place.clone();
            let worktree = row.worktree.then_some(());
            let activity = row.activity.clone();
            // Once a turn is over and summarised: its headline, how it ended when it needs the
            // person or went wrong.
            let summary = row.summary.clone().filter(|_| crate::summaries_on(cx));
            // A context filling up shows amber, then red, as in the session's header.
            let cost = row.spend(cx).map(|sp| {
                let color = match sp {
                    Spend::Context { fill: Fill::Filling, .. } => cx.theme().warning,
                    Spend::Context { fill: Fill::Nearly, .. } => cx.theme().danger,
                    _ => cx.theme().muted_foreground,
                };
                (sp.short(), color)
            });
            let short_id = row.short_id.clone();
            let dot = self.dot(ix, cx);
            let key = self.key(ix);
            let auto = self.list_name(ix, cx);
            let current = self.name(ix, cx);
            let title = panels::title_element(&key, auto.clone(), None, None, cx);
            let drag = SessionDrag { number, name: current.clone().into() };
            let (k1, a1, c1) = (key, auto, current);
            let menu = self.session_menu(ix, cx);
            col = col.child(
                div()
                    .id(("session-row", number))
                    .group(SharedString::from(format!("session-row-{number}")))
                    .relative()
                    .mx(px(6.))
                    .px_2()
                    .py(px(6.))
                    .flex()
                    .gap_2()
                    .rounded(px(8.))
                    .cursor_pointer()
                    // The shown session: a rounded highlight.
                    .when(active, |d| d.bg(sel))
                    .when(!active, |d| d.hover(|d| d.bg(hover)))
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
                                    .children(cost.map(|(c, color)| div().flex_none().text_size(px(10.5)).text_color(color).child(c))),
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
                            .children(match summary {
                                Some((status, needs_you, headline)) => {
                                    use keleuo_core::summary::Status;
                                    // How the work went when it is not plainly done, else that
                                    // something waits on the person.
                                    let (label, tone) = match status {
                                        Status::Done if needs_you => (Some("Needs you"), yellow),
                                        Status::Done => (None, yellow),
                                        Status::Waiting => (Some(status.label()), yellow),
                                        Status::Failed => (Some(status.label()), red),
                                        Status::Partial => (Some(status.label()), blue),
                                    };
                                    Some(
                                        div()
                                            .flex()
                                            .gap_1()
                                            .text_size(px(11.))
                                            .children(label.map(|l| div().flex_none().text_color(tone).child(l)))
                                            .child(div().min_w_0().truncate().text_color(muted.opacity(0.9)).child(headline)),
                                    )
                                }
                                None => activity.map(|a| div().flex().text_size(px(11.)).text_color(muted.opacity(0.8)).child(div().min_w_0().truncate().child(a))),
                            }),
                    )
                    // Rename and close, on hover (right-click is easy to miss on a Mac).
                    .child({
                        let (k2, a2, c2) = (k1.clone(), a1.clone(), c1.clone());
                        div()
                            .absolute()
                            .top(px(4.))
                            .right(px(4.))
                            .flex()
                            .rounded(px(6.))
                            .bg(if active { sel } else { hover })
                            .invisible()
                            .group_hover(SharedString::from(format!("session-row-{number}")), |d| d.visible())
                            .child(
                                Button::new(("session-rename", number))
                                    .icon(gpui_kit::assets::IconName::Pencil)
                                    .xsmall()
                                    .ghost()
                                    .tooltip("Rename")
                                    .on_click(move |_, window, cx| {
                                        cx.stop_propagation();
                                        start_tab_rename(k2.clone(), a2.clone(), c2.clone(), window, cx);
                                    }),
                            )
                            .child(
                                Button::new(("session-close", number))
                                    .icon(IconName::Close)
                                    .xsmall()
                                    .ghost()
                                    .tooltip("Close the session (its history stays)")
                                    .on_click(cx.listener(move |s, _, window, cx| {
                                        cx.stop_propagation();
                                        if let Some(ix) = s.tabs.iter().position(|t| t.number == number) {
                                            s.request_close(ix, window, cx);
                                        }
                                    })),
                            )
                    })
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
                    .context_menu(menu),
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
        // A lighter panel set into the window's grey frame, rounded at its top left (as Codex's
        // sidebar is): a heading, then New session as a row of its own.
        div()
            .w(px(LIST_WIDTH))
            .flex_none()
            .h_full()
            .ml(px(6.))
            .relative()
            .flex()
            .flex_col()
            .bg(crate::theme::tool_ground(cx))
            .child(
                div()
                    .flex_none()
                    .h(px(40.))
                    .px(px(14.))
                    .flex()
                    .items_center()
                    .text_size(px(18.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(fg)
                    .child("Sessions"),
            )
            .child(
                div()
                    .id("session-new")
                    .h(px(32.))
                    .flex_none()
                    .mx(px(6.))
                    .mb_1()
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(8.))
                    .bg(bg)
                    .text_size(px(14.))
                    .text_color(fg)
                    .cursor_pointer()
                    .hover(|d| d.bg(hover))
                    .child(Icon::new(IconName::Plus).small())
                    .child("New session")
                    .child(div().flex_1())
                    .child(div().text_xs().text_color(muted.opacity(0.7)).child("Cmd+N"))
                    .on_click(cx.listener(|s, _, window, cx| s.new_session(window, cx))),
            )
            .child(col)
            .child(crate::dock_skin::corner_cap(true, true, bg))
            .into_any_element()
    }

    /// The folded list: a dot and initials per session, with the name in a tooltip.
    fn render_rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, hover, ring, bg, sel) =
            (theme.foreground, theme.muted_foreground, theme.secondary_hover, theme.ring, theme.sidebar, theme.secondary);
        let mut col = div()
            .id("session-rail")
            .w(px(RAIL_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(RAIL_GAP))
            .py(px(RAIL_GAP))
            .bg(bg)
            // A press here is meant for what is under it: the list does not come out over it.
            .on_mouse_down(MouseButton::Left, cx.listener(|s, _, _, _| s.peek_held = true))
            .on_mouse_down(MouseButton::Right, cx.listener(|s, _, _, _| s.peek_held = true))
            .child(
                div()
                    .id("rail-new")
                    .size(px(RAIL_NEW))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .hover(|d| d.bg(hover))
                    .child(Icon::new(IconName::Plus).small().text_color(muted))
                    .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("New session (Cmd+N)").build(w, cx))
                    .on_click(cx.listener(|s, _, window, cx| s.new_session(window, cx))),
            )
            // Resting on the rail shows the whole list over the content; leaving it hides it.
            .on_hover(cx.listener(|s, hovered: &bool, _, cx| {
                s.rail_hover = *hovered;
                if !*hovered {
                    s.peek_held = false;
                }
                if *hovered {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(std::time::Duration::from_millis(PEEK_DELAY_MS)).await;
                        let _ = this.update(cx, |s, cx| {
                            if s.rail_hover && s.folded && !s.peek && !s.peek_held {
                                s.peek = true;
                                cx.notify();
                            }
                        });
                    })
                    .detach();
                }
            }));
        for ix in 0..self.tabs.len() {
            let active = ix == self.active && self.browser.is_none();
            let number = self.tabs[ix].number;
            let name = self.name(ix, cx);
            let activity = self.tabs[ix].row.activity.clone();
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
                    .size(px(RAIL_ROW))
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
                    }))
                    .context_menu(self.session_menu(ix, cx)),
            );
        }
        col.into_any_element()
    }

    /// The list that peeks out over the rail: the rail made wide. Each session keeps its row
    /// (the same height, its two letters in the same place) with its name and what it last did
    /// beside them, so what the pointer was over is what it is over now.
    fn render_peek(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, hover, ring, bg, sel) =
            (theme.foreground, theme.muted_foreground, theme.secondary_hover, theme.ring, theme.sidebar, theme.secondary);
        // The rail centres its items: the left edge they start from.
        let inset = |w: f32| px((RAIL_WIDTH - w) / 2.);
        let mut col = div()
            .id("session-peek-list")
            .w(px(PEEK_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .gap(px(RAIL_GAP))
            .py(px(RAIL_GAP))
            .bg(bg)
            .child(
                div()
                    .id("peek-new")
                    .h(px(RAIL_NEW))
                    .mx(px(4.))
                    .pl(inset(RAIL_NEW) - px(4.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_md()
                    .text_sm()
                    .text_color(muted)
                    .hover(|d| d.bg(hover))
                    .child(div().size(px(RAIL_NEW)).flex().items_center().justify_center().child(Icon::new(IconName::Plus).small()))
                    .child("New session")
                    .on_click(cx.listener(|s, _, window, cx| s.new_session(window, cx))),
            );
        for ix in 0..self.tabs.len() {
            let active = ix == self.active && self.browser.is_none();
            let number = self.tabs[ix].number;
            let name = self.name(ix, cx);
            // The name, or the field renaming it.
            let title = panels::title_element(&self.key(ix), self.list_name(ix, cx), None, None, cx);
            let row = &self.tabs[ix].row;
            let detail = row.activity.clone().unwrap_or_else(|| row.folder.clone());
            let dot = self.dot(ix, cx);
            col = col.child(
                div()
                    .id(("peek-session", number))
                    .h(px(RAIL_ROW))
                    .mx(px(4.))
                    .pl(inset(RAIL_ROW) - px(4.))
                    .pr_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_md()
                    .when(active, |d| d.bg(sel))
                    .when(!active, |d| d.hover(|d| d.bg(hover)))
                    .child(
                        div()
                            .relative()
                            .flex_none()
                            .size(px(RAIL_ROW))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if active { fg } else { muted })
                            .when(active, |d| d.border_1().border_color(ring))
                            .child(initials(&name))
                            .children(dot.map(|(c, _)| {
                                div().absolute().top(px(2.)).right(px(2.)).size(px(8.)).rounded_full().border_1().border_color(bg).bg(c)
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().text_size(px(12.5)).line_height(px(15.)).text_color(fg).child(title))
                            .child(div().truncate().text_size(px(10.5)).line_height(px(13.)).text_color(muted).child(detail)),
                    )
                    .on_click(cx.listener(move |s, _, window, cx| {
                        if let Some(ix) = s.tabs.iter().position(|t| t.number == number) {
                            s.activate(ix, window, cx);
                        }
                    }))
                    .context_menu(self.session_menu(ix, cx)),
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
    let out = keleuo_core::platform::command("git").arg("-C").arg(folder).args(["rev-parse", "--show-toplevel"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let top = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let branch = keleuo_core::host::LocalHost.git_branch(&top);
    Some((top, branch))
}

/// A free name for a worktree of the repository at `top`: `<repo>-2`, `<repo>-3`, ... (a
/// folder next to the repository and a `keleuo/<name>` branch, neither existing yet).
pub fn suggest_worktree_name(top: &Path) -> String {
    let name = folder_name(top);
    let parent = top.parent().unwrap_or(top);
    (2..).map(|n| format!("{name}-{n}")).find(|c| !parent.join(c).exists() && !branch_exists(top, &format!("keleuo/{c}"))).unwrap_or(name)
}

fn branch_exists(top: &Path, branch: &str) -> bool {
    keleuo_core::platform::command("git")
        .arg("-C")
        .arg(top)
        .args(["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// `git worktree add -b keleuo/<name> <parent of top>/<name>`: the folder and branch the
/// person saw and named. Refuses a name already taken.
fn make_worktree(top: &Path, name: &str) -> Result<(PathBuf, String), String> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return Err(format!("\"{name}\" is not a usable folder name"));
    }
    let parent = top.parent().ok_or("the repository has no parent folder")?;
    let path = parent.join(name);
    let branch = format!("keleuo/{name}");
    if path.exists() {
        return Err(format!("{} exists already", path.display()));
    }
    if branch_exists(top, &branch) {
        return Err(format!("the branch {branch} exists already"));
    }
    let out = keleuo_core::platform::command("git")
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
        crate::perf::count(crate::perf::Count::Frame);
        let theme = cx.theme();
        // The window's chrome (title bar, session list) is the grey the work area sits in.
        let (bg, fg, hover, muted) = (theme.sidebar, theme.foreground, theme.secondary_hover, theme.muted_foreground);
        // Blocking the mouse: on Windows the title bar is a drag area, and a click the button
        // does not block reaches it (Windows then moves the window instead).
        let button = |id: &'static str, icon: Icon, tip: &'static str| {
            div()
                .id(id)
                .occlude()
                .size(px(22.))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded_sm()
                .hover(|d| d.bg(hover))
                .child(icon.small().text_color(muted))
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip).build(window, cx))
        };
        let to_ws = |f: fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
            cx.listener(move |s: &mut Shell, _: &ClickEvent, window: &mut Window, cx: &mut Context<Shell>| {
                if let Some(ws) = s.active_workspace() {
                    ws.update(cx, |w, cx| f(w, window, cx));
                }
            })
        };
        let showing_settings = self.settings.is_some();
        let title = if showing_settings {
            "Settings".to_string()
        } else if self.browser.is_some() || self.tabs.is_empty() {
            "New session".to_string()
        } else {
            self.name(self.active, cx)
        };
        // The session's own buttons, only while it shows.
        let has_session = !self.tabs.is_empty() && self.browser.is_none() && !showing_settings;
        // The tool panels by name, each opening the sidebar on it: what the icon strip beside
        // the folded sidebar did.
        let tools_menu = {
            let ws = self.active_workspace().map(|w| w.downgrade());
            // Read when the menu opens, not on every frame.
            move |mut menu: PopupMenu, _: &mut Window, cx: &mut Context<PopupMenu>| {
                let tools = ws.as_ref().and_then(|w| w.upgrade()).map(|w| w.read(cx).tool_panels(cx)).unwrap_or_default();
                for (name, id) in tools.iter().copied() {
                    let ws = ws.clone();
                    menu = menu.item(PopupMenuItem::new(name).on_click(move |_, window, cx| {
                        if let Some(ws) = ws.as_ref() {
                            let _ = ws.update(cx, |w, cx| w.show_panel(id, window, cx));
                        }
                    }));
                }
                menu
            }
        };
        // The whole menu (Appearance in it), from a button at the right end, as Chrome has it.
        let menu_button = gpui_kit::component::button::Button::new("t-menu")
            .icon(Icon::new(gpui_kit::assets::IconName::EllipsisVertical))
            .ghost()
            .xsmall()
            .dropdown_menu_with_anchor(Anchor::TopRight, crate::actions::menu_popup);
        // Room for the macOS window buttons at the left; Windows has its own at the right.
        let left = if cfg!(target_os = "macos") { 76. } else { 8. };
        let top = TitleBar::new().h(px(34.)).pl(px(left)).child(
            div()
                .size_full()
                .relative()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    button("t-sessions", Icon::new(IconName::PanelLeft), "Session list (Cmd+Shift+B)")
                        .on_click(cx.listener(|s, _, _, cx| s.toggle_folded(cx))),
                )
                .child(
                    div().flex_1().min_w_0().child(
                        div()
                            .max_w(relative(0.6))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_xs()
                            .text_color(muted)
                            .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(fg).child("Keleuo"))
                            .child(div().flex_none().child("·"))
                            .child(div().min_w_0().truncate().child(title)),
                    ),
                )
                .when(has_session, |d| {
                    d.child(
                        div()
                            .h_full()
                            .flex()
                            .items_center()
                            .gap_1()
                            .pr_1()
                            .child(
                                button("t-terminal", Icon::new(IconName::SquareTerminal), "New terminal (Cmd+T)")
                                    .on_click(to_ws(|w, win, cx| w.new_terminal(win, cx))),
                            )
                            .child(
                                button("t-sidebar", Icon::new(IconName::PanelLeft), "Tool panels (Cmd+B; right-click for one)")
                                    .on_click(to_ws(|w, win, cx| w.toggle_sidebar_pub(win, cx)))
                                    .context_menu(tools_menu),
                            )
                            .child(
                                button("t-split", Icon::new(IconName::PanelRight), "Split the shown file or terminal (Cmd+\\)")
                                    .on_click(to_ws(|w, win, cx| w.split_right(win, cx))),
                            )
                            .child(
                                button("t-reset", Icon::new(IconName::RotateCw), "Reset layout")
                                    .on_click(to_ws(|w, win, cx| w.reset_layout(win, cx))),
                            ),
                    )
                })
                // Settings: pressed while open; again goes back.
                .child(
                    button("t-settings", Icon::new(IconName::Settings), "Settings (Cmd+,)")
                        .when(showing_settings, |d| d.bg(hover))
                        .on_click(cx.listener(|s, _, window, cx| {
                            if s.settings.is_some() {
                                s.close_settings(window, cx)
                            } else {
                                s.show_settings(window, cx)
                            }
                        })),
                )
                .child(div().flex_none().pr_1().occlude().child(menu_button)),
        );
        let main = match (&self.settings, &self.browser) {
            (Some((page, _)), _) => page.clone().into_any_element(),
            (None, Some(b)) => b.clone().into_any_element(),
            (None, None) => self.active_workspace().map(|w| w.into_any_element()).unwrap_or_else(|| div().into_any_element()),
        };
        let list = if self.tabs.is_empty() {
            None
        } else if self.folded {
            Some(self.render_rail(cx))
        } else {
            Some(self.render_list(cx))
        };
        let list_shown = list.is_some();
        let folder_rename = self.render_folder_rename(cx);
        // Web pages are native views over the window: they hide while an overlay is open.
        crate::web_panel::set_covered(
            self.folder_rename.is_some() || (self.folded && self.peek && !self.tabs.is_empty()) || crate::image_view::is_open(cx)
                || crate::skill_installer::is_open(cx),
        );
        // The folded list, peeking out over the content until the pointer leaves it.
        let peek = (self.folded && self.peek && !self.tabs.is_empty()).then(|| {
            div()
                .id("session-peek")
                .absolute()
                .top_0()
                .left_0()
                .h_full()
                .occlude()
                .shadow_lg()
                .child(self.render_peek(cx))
        });
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(bg)
            .text_color(fg)
            // A touch heavier than regular, as Codex sets SF Pro (430): denser, still not bold.
            .font_weight(FontWeight(crate::theme::UI_WEIGHT))
            // An image opened full size: Esc closes it, wherever the keyboard is.
            .capture_key_down(cx.listener(|_, ev: &KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" && crate::image_view::is_open(cx) {
                    crate::image_view::close(cx);
                    cx.stop_propagation();
                } else if ev.keystroke.key == "escape" && crate::skill_installer::is_open(cx) {
                    crate::skill_installer::close(cx);
                    cx.stop_propagation();
                }
            }))
            .track_focus(&self.focus)
            .on_action(cx.listener(|s, _: &NewSession, w, cx| s.new_session(w, cx)))
            .on_action(cx.listener(|s, _: &ToggleSessions, _, cx| s.toggle_folded(cx)))
            .on_action(cx.listener(|s, _: &CloseSession, w, cx| {
                let ix = s.active;
                s.request_close(ix, w, cx)
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
            // Cmd+O: the browser, with the system's folder picker over it.
            .on_action(cx.listener(|s, _: &OpenFolder, w, cx| {
                s.new_session(w, cx);
                if let Some(b) = s.browser.clone() {
                    b.update(cx, |b, cx| b.choose_folder(w, cx));
                }
            }))
            .on_action(cx.listener(|s, _: &RenameFolder, w, cx| {
                if let Some(ws) = s.active_workspace() {
                    let (host, root) = {
                        let w = ws.read(cx);
                        (w.host(), w.root().to_path_buf())
                    };
                    if !host.is_remote() {
                        s.ask_rename_folder(root, w, cx);
                    }
                }
            }))
            .on_action(cx.listener(|s, _: &RenameSession, w, cx| {
                if s.tabs.is_empty() {
                    return;
                }
                let ix = s.active;
                let (key, auto, current) = (s.key(ix), s.list_name(ix, cx), s.name(ix, cx));
                start_tab_rename(key, auto, current, w, cx);
            }))
            .child(top)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .flex()
                    .children(list)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            // The peeking list goes when the pointer moves on over the content
                            // (not when it merely leaves the list: a menu opened from the list
                            // floats outside it, and holds the pointer while it is open).
                            .when(self.peek, |d| {
                                d.on_mouse_move(cx.listener(|s, _: &MouseMoveEvent, _, cx| {
                                    // A session being renamed in it keeps it out.
                                    let renaming = cx
                                        .global::<panels::Renaming>()
                                        .0
                                        .as_ref()
                                        .is_some_and(|r| s.tabs.iter().any(|t| panels::session_key(t.number) == r.key));
                                    if s.peek && !renaming {
                                        s.peek = false;
                                        s.rail_hover = false;
                                        cx.notify();
                                    }
                                }))
                            })
                            .relative()
                            .child(main)
                            // Beside the folded rail (the frame), the work area is rounded at its
                            // top left; beside the open list, the list's corner is.
                            .when(list_shown && self.folded, |d| d.child(crate::dock_skin::corner_cap(true, true, bg))),
                    )
                    .children(peek),
            )
            .children(folder_rename)
            .children(crate::skill_installer::render(cx))
            .children(crate::image_view::render(cx))
    }
}

/// A session closed for a folder rename, to open again: where, with what, and its id.
type Reopen = (Arc<dyn Host>, PathBuf, Arc<dyn Backend>, Option<String>);

/// A folder being renamed: its new name in the input, and what is wrong with it, if anything.
struct FolderRename {
    folder: PathBuf,
    input: Entity<InputState>,
    error: Option<String>,
    _sub: Subscription,
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

/// Keleuo > Settings… and Cmd+,: Settings in the window.
pub fn open_settings(cx: &mut App) {
    let Some((handle, shell)) = cx.try_global::<crate::control::ControlTargets>().and_then(|t| t.workspace.clone()) else { return };
    // After the key press or menu click that asked, which still holds the window.
    cx.defer(move |cx| {
        let _ = handle.update(cx, |_, window, cx| {
            let _ = shell.update(cx, |s, cx| s.show_settings(window, cx));
        });
    });
}
