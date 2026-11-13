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
use gpui_kit::component::resizable::{resizable_panel, v_resizable};
use std::rc::Rc;
use gpui_kit::component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, TitleBar, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::*;
use wundur_core::agent::{LiveReason, LiveSession};
use wundur_core::agent::Backend;
use wundur_core::host::{Host, LocalHost};

use crate::AppSettings;
use wundur_core::settings::SavedSession;
use crate::actions::*;
use crate::agent_session::{AgentSession, Fill, SessionEvent, Spend};
use crate::attention::{self, Action, Need};
use crate::browser::{Browser, BrowserEvent, OpenSession};
use crate::panels::{self, start_tab_rename};
use crate::workspace::{Workspace, WorkspaceEvent};

/// The left column's width limits (dragged by its right edge; kept in the layout settings).
const LIST_MIN: f32 = 200.;
const LIST_MAX: f32 = 520.;
/// The tool panels' share of the column under the sessions, to start with.
const TOOLS_HEIGHT: f32 = 340.;
/// The toolbar along the window's left edge (Projects, Inbox, Settings).
const TOOLBAR_WIDTH: f32 = 44.;
/// The inbox's list, over the content beside the toolbar.
const INBOX_WIDTH: f32 = 360.;
/// What a session that was working when Wundur quit is told when it starts again.
const CONTINUE_MESSAGE: &str = "Wundur was restarted while you were working. Continue where you left off.";

/// No notifications, Dock bounces or badge: in tests, and in the background (test) mode.
fn silent() -> bool {
    cfg!(test) || crate::background()
}

struct Tab {
    number: usize,
    ws: Entity<Workspace>,
    /// What its row shows, kept up to date as the session changes (see [`Row`]).
    row: Row,
    /// When it last did something: its transcript's time when it was opened, then each time
    /// it starts or stops working or does something new.
    changed: Option<std::time::SystemTime>,
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
    /// The machine, for a session on another one.
    host: Option<String>,
    /// The folder's git branch, when it has one.
    branch: Option<String>,
    folder: String,
    /// Folder, branch and machine.
    place: String,
    worktree: bool,
    activity: Option<String>,
    /// The last turn's summary, once the turn is over: status, whether it needs the person,
    /// headline.
    summary: Option<(wundur_core::summary::Status, bool, String)>,
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
            host: w.host().is_remote().then(|| w.host().label().to_string()),
            branch: w.branch().map(|b| b.to_string()),
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
    /// The inbox's list is open over the content (the toolbar's Inbox).
    inbox_open: bool,
    /// A notice being answered in the person's own words: its id and the field.
    inbox_reply: Option<(u64, Entity<InputState>, Subscription)>,
    /// The folder being renamed, asked for its new name.
    folder_rename: Option<FolderRename>,
    /// A folder being made a project: the name asked for first.
    project_form: Option<ProjectForm>,
    /// Go to session (Cmd+K): what is typed, and the match picked.
    switcher: Option<Switcher>,
    /// The tool panel shown under the sessions (Files, Changes, Agent, Skills).
    tools_tab: &'static str,
    /// The left column's right edge is being dragged.
    list_resizing: bool,
    /// Its tabs along the bottom (drawn as the documents' are).
    tools_strip: Rc<crate::sheet_tabs::StripState>,
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
    earlier: Vec<(wundur_core::agent::SessionInfo, Option<u32>)>,
    /// What each session waits on the person for, to tell them once (see `attention`).
    attention: attention::Tracker,
    /// The count on the Dock icon.
    badge: usize,
    _attention: Vec<Subscription>,
}

impl Shell {
    pub fn new(start: Start, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let folded = cx.global::<AppSettings>().0.borrow().sessions_folded;
        // The list says how long ago each session did something: drawn again once a minute so
        // that stays true while nothing else changes.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(std::time::Duration::from_secs(60)).await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        })
        .detach();
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
            inbox_open: false,
            inbox_reply: None,
            folder_rename: None,
            project_form: None,
            switcher: None,
            tools_tab: "Files",
            list_resizing: false,
            tools_strip: Rc::new(crate::sheet_tabs::StripState::default()),
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
        // The shown session first, so the window has it at once; the others after it, one at a
        // time off the window's thread, each put in its place in the list as it comes.
        let mut order: Vec<(usize, SavedSession)> = saved.into_iter().enumerate().collect();
        if let Some(first) = shown.and_then(|n| order.iter().position(|(_, s)| s.number == n)) {
            let s = order.remove(first);
            order.insert(0, s);
        }
        let shown_number = order.first().map(|(_, s)| s.number);
        // The shown one's workspace at once (its files, terminals, its row), its chat showing
        // its shape until its conversation is read.
        let mut placed: Vec<usize> = Vec::new();
        let mut first: Option<(SavedSession, Entity<AgentSession>)> = None;
        if let Some((place, s)) = order.first().cloned().filter(|(_, s)| s.folder.is_dir()) {
            order.remove(0);
            let host: Arc<dyn Host> = Arc::new(LocalHost);
            let backend = backends.iter().find(|b| b.id() == s.backend).cloned().unwrap_or_else(|| fallback.clone());
            let mut session = AgentSession::new(host.clone(), backend, s.folder.clone());
            session.dormant = true;
            session.loading_chat = s.id.is_some();
            session.chat.session_id = s.id.clone();
            session.writes_anywhere = s.writes_anywhere;
            session.writable = s.writable.clone();
            let next = self.next;
            self.next = s.number.max(1);
            let ix = self.insert_session(host, s.folder.clone(), session, None, None, 0, window, cx);
            self.next = next.max(s.number + 1);
            self.activate(ix, window, cx);
            placed.push(place);
            first = Some((s, self.tabs[ix].ws.read(cx).session.clone()));
        }
        cx.spawn_in(window, async move |this, cx| {
            if let Some((s, session)) = first {
                let backend = Some(session.read_with(cx, |s, _| s.backend.clone()));
                let read = cx
                    .background_spawn(async move {
                        let (backend, id) = (backend?, s.id.clone()?);
                        backend.load_session(&LocalHost, &s.folder, &id).ok()
                    })
                    .await;
                session.update(cx, |s, cx| match read {
                    Some(chat) => s.take_chat(chat, cx),
                    None => {
                        s.loading_chat = false;
                        cx.notify();
                    }
                });
            }
            for (place, s) in order {
                let (backends, fallback) = (backends.clone(), fallback.clone());
                let loaded = cx
                    .background_spawn(async move {
                        if !s.folder.is_dir() {
                            return None;
                        }
                        let backend = backends.iter().find(|b| b.id() == s.backend).cloned().unwrap_or_else(|| fallback.clone());
                        let chat = match &s.id {
                            Some(id) => Some(backend.load_session(&LocalHost, &s.folder, id).ok()?),
                            None => None,
                        };
                        Some((s, backend, chat))
                    })
                    .await;
                let Some((s, backend, chat)) = loaded else { continue };
                let at = placed.iter().filter(|p| **p < place).count();
                placed.push(place);
                let gone = this
                    .update_in(cx, |shell, window, cx| {
                        let host: Arc<dyn Host> = Arc::new(LocalHost);
                        let mut session = match chat {
                            Some(chat) => AgentSession::resumed(host.clone(), backend, s.folder.clone(), chat, false),
                            None => AgentSession::new(host.clone(), backend, s.folder.clone()),
                        };
                        session.dormant = true;
                        session.writes_anywhere = s.writes_anywhere;
                        session.writable = s.writable.clone();
                        let next = shell.next;
                        shell.next = s.number.max(1);
                        let ix = shell.insert_session(host, s.folder, session, None, None, at, window, cx);
                        shell.next = next.max(s.number + 1);
                        if Some(s.number) == shown_number {
                            shell.activate(ix, window, cx);
                        }
                        cx.notify();
                    })
                    .is_err();
                if gone {
                    return;
                }
            }
            let _ = this.update_in(cx, |shell, window, cx| {
                shell.restoring = false;
                if shell.tabs.is_empty() || shell.active >= shell.tabs.len() {
                    shell.active = 0;
                }
                shell.handle_start(start, window, cx);
                shell.save_open(cx);
                shell.continue_working(cx);
            });
        })
        .detach();
    }

    /// Sessions that were working when Wundur quit, and the person asked to carry on: each is
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

    /// Quitting: when sessions are working, ask whether they should carry on when Wundur starts
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
                    // Only commands: they stop with Wundur, and there is nothing to carry on.
                    (format!("{what}. Quit anyway?"), "The command stops when Wundur quits.".to_string(), vec![
                        PromptButton::Ok("Quit".into()),
                        PromptButton::Cancel("Cancel".into()),
                    ])
                } else {
                    let (them, they, carry, wait) =
                        if names.len() > 1 { ("them", "they", "carry", "wait") } else { ("it", "it", "carries", "waits") };
                    (
                        format!("{what}. Continue {them} when Wundur starts again?"),
                        format!(
                            "{}\n\nYes: {they} {carry} on when Wundur starts again.\nNo: quit; {they} {wait} for your next message.",
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
                        eprintln!("wundur: session read in {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
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
        chat: std::io::Result<wundur_core::conversation::Conversation>,
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
        let ix = self.insert_session(host, root, session, worktree, wait_for, self.tabs.len(), window, cx);
        self.activate(ix, window, cx);
        self.save_open(cx);
    }

    /// Add a session's tab at `at` (in the list), without showing it; its index.
    #[allow(clippy::too_many_arguments)]
    fn insert_session(
        &mut self,
        host: Arc<dyn Host>,
        root: PathBuf,
        session: AgentSession,
        worktree: Option<String>,
        wait_for: Option<u32>,
        at: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
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
                WorkspaceEvent::FreshStart => shell.fresh_from_summary(ws.clone(), window, cx),
                WorkspaceEvent::ShowTool(name) => {
                    // Only the shown session's tools are drawn.
                    if shell.active_workspace().as_ref() == Some(ws) {
                        shell.show_tool(name, cx);
                    }
                }
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
        let at = at.min(self.tabs.len());
        let changed = transcript_time(ws.read(cx), &row, cx);
        self.tabs.insert(at, Tab { number, ws, row, changed });
        if at <= self.active && self.tabs.len() > 1 {
            self.active += 1;
        }
        at
    }

    pub fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        // Picking a session leaves Settings.
        self.settings = None;
        self.active = ix;
        self.browser = None;
        self._browser_sub = None;
        let ws = self.tabs[ix].ws.clone();
        crate::perf::switch_begin("session");
        ws.update(cx, |w, cx| {
            w.session.update(cx, |s, cx| {
                s.unseen = false;
                // Reopened from the last run with its Terminal showing: start it now.
                s.wake(cx);
            });
            crate::perf::switch_step("woken");
            w.focus_main(window, cx);
        });
        crate::perf::switch_step("focused");
        // Seen now: its row loses the mark.
        self.refresh_row(&ws, cx);
        self.save_open(cx);
        crate::perf::switch_step("saved");
        let title = format!("{} - Wundur", self.name(ix, cx));
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
        if tab.row.dot != row.dot || tab.row.activity != row.activity || tab.row.summary != row.summary {
            tab.changed = Some(std::time::SystemTime::now());
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
        let session_id = row.short_id.clone();
        let weak = cx.entity().downgrade();
        move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            let (w1, w3) = (weak.clone(), weak.clone());
            let (k, a, c, k2) = (key.clone(), auto.clone(), current.clone(), key.clone());
            let path = folder.display().to_string();
            menu.item(PopupMenuItem::new("Rename").on_click(move |_, window, cx| {
                start_tab_rename(k.clone(), a.clone(), c.clone(), window, cx);
                // Hidden, the list shows again so its name can be edited there.
                let _ = w3.update(cx, |s, cx| {
                    if s.folded {
                        s.toggle_folded(cx);
                    }
                });
            }))
            .item(PopupMenuItem::new("Reset name").on_click(move |_, window, cx| panels::reset_tab_name(&k2, window, cx)))
            .separator()
            .when(!remote, |m| {
                let (w2, f, f2) = (weak.clone(), folder.clone(), folder.clone());
                let f3 = folder.clone();
                let w3 = weak.clone();
                m.item(PopupMenuItem::new("Make a project of this folder…").on_click(move |_, window, cx| {
                    let _ = w3.update(cx, |s, cx| s.ask_new_project(f3.clone(), window, cx));
                }))
                    .item(PopupMenuItem::new("Reveal in Finder").on_click(move |_, _, cx| cx.reveal_path(&f2)))
                    .item(PopupMenuItem::new("Rename folder…").on_click(move |_, window, cx| {
                        let _ = w2.update(cx, |s, cx| s.ask_rename_folder(f.clone(), window, cx));
                    }))
            })
            .item(PopupMenuItem::new("Copy folder path").on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(path.clone()))
            }))
            .when_some(session_id.clone(), |m, id| {
                m.item(PopupMenuItem::new("Copy session ID").on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(id.clone()))))
            })
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
        window.set_window_title(&format!("{} - Wundur", self.name(self.active, cx)));
        cx.notify();
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

    /// A reviewer for a session (see `messaging`): a new read-only session of `backend` in
    /// `root`, named `name`, added to the list without leaving the shown session. Its number
    /// and session.
    pub fn open_reviewer(
        &mut self,
        root: PathBuf,
        backend: Arc<dyn Backend>,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(usize, Entity<AgentSession>)> {
        let shown = self.active;
        let mut session = AgentSession::new(Arc::new(LocalHost), backend, root.clone());
        session.make_reviewer();
        self.add_session(Arc::new(LocalHost), root, session, None, None, window, cx);
        let tab = self.tabs.last()?;
        let (number, session) = (tab.number, tab.ws.read(cx).session.clone());
        let _ = cx.global::<panels::Names>().0.borrow_mut().set(&panels::session_key(number), name);
        if shown < self.tabs.len() - 1 {
            self.activate(shown, window, cx);
        }
        Some((number, session))
    }

    /// Carry on from the session in `ws` in a new one (same folder, agent and model), its
    /// message box holding a handoff drafted from the old one (see `wundur_core::handoff`),
    /// condensed by the local summary model when one is set up. The old session is left as
    /// it is.
    pub fn fresh_from_summary(&mut self, ws: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) {
        let number = self.tabs.iter().find(|t| t.ws == ws).map(|t| t.number);
        let given = number.and_then(|n| cx.global::<panels::Names>().0.borrow().get(&panels::session_key(n)).map(str::to_string));
        let (host, root, old) = {
            let w = ws.read(cx);
            (w.host(), w.root().to_path_buf(), w.session.clone())
        };
        let (draft, backend, model) = {
            let s = old.read(cx);
            (wundur_core::handoff::draft(&s.chat, &root, given.as_deref().unwrap_or("")), s.backend.clone(), s.model.clone())
        };
        let mut session = AgentSession::new(host.clone(), backend, root.clone());
        session.model = model;
        self.add_session(host, root, session, None, None, window, cx);
        let Some(tab) = self.tabs.last() else { return };
        let (fresh, chat) = {
            let w = tab.ws.read(cx);
            (w.session.clone(), w.claude.clone())
        };
        if let Some(name) = given {
            let _ = cx.global::<panels::Names>().0.borrow_mut().set(&panels::session_key(tab.number), &format!("{name} (fresh)"));
        }
        let summaries = cx.global::<AppSettings>().0.borrow().summaries.clone();
        if summaries.model.trim().is_empty() {
            chat.update(cx, |p, cx| p.take_handoff(&draft, window, cx));
            return;
        }
        // With a local model: condensed into a brief (the draft when that fails).
        fresh.update(cx, |s, cx| {
            s.chat.push_notice(format!("Writing the handoff with {}…", summaries.model.trim()));
            cx.notify();
        });
        let window_handle = window.window_handle();
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            let d = draft.clone();
            let condensed = cx.background_spawn(async move { wundur_core::handoff::condense(&summaries, &d) }).await;
            let text = match condensed {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("wundur: handoff: {e:#}");
                    draft
                }
            };
            let _ = window_handle.update(cx, |_, window, cx| chat.update(cx, |p, cx| p.take_handoff(&text, window, cx)));
        })
        .detach();
    }

    /// The new-session screen for the folder of the project keyed `root` (its name, agent and
    /// model to choose, ready to start), on the machine of the project's latest session.
    fn new_session_for_project(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let latest = self.tabs.iter().rev().find(|t| project_root_of(&t.row).starts_with(&root)).map(|t| t.ws.clone());
        let host = match &latest {
            Some(ws) => ws.read(cx).host(),
            None => Arc::new(LocalHost) as Arc<dyn Host>,
        };
        // A project on another machine is keyed `ssh://host/folder`: its folder is the rest.
        let folder = match root.to_string_lossy().strip_prefix("ssh://") {
            Some(rest) => {
                if latest.is_none() {
                    return;
                }
                PathBuf::from(rest.find('/').map_or("/", |i| &rest[i..]))
            }
            None => root,
        };
        self.open_browser(host, folder, None, window, cx);
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
            "browse-off" => {
                // Tests: close the New session screen (the session under it shows).
                self.close_browser(window, cx);
                "ok".into()
            }
            "new-agent" => {
                // Tests: `new-agent <backend id>`: a session on that agent, in the shown folder.
                let Some((host, root)) = self.active_workspace().map(|w| (w.read(cx).host(), w.read(cx).root().to_path_buf())) else {
                    return Some("no session".into());
                };
                let b = crate::backends::backend(Some(arg), cx);
                self.new_session_in(host, root, b, window, cx);
                "ok".into()
            }
            "font-search" | "font-pick" => {
                // Tests: `font-search app|chat|terminal [text]` opens a font's search in the
                // shown Settings and types `text`; `font-pick <which> <family>` picks a Google
                // Font (once the catalogue is in: ask again until it says ok).
                let (w, rest) = arg.split_once(' ').unwrap_or((arg, ""));
                let which = match w {
                    "chat" => crate::fonts::Which::Chat,
                    "terminal" => crate::fonts::Which::Terminal,
                    _ => crate::fonts::Which::App,
                };
                if cmd == "font-pick" {
                    crate::fonts::ask_catalogue(cx);
                    let Some(f) = crate::fonts::matches(which, rest, 50, cx).into_iter().find(|f| f.name == rest) else {
                        return Some("not in the catalogue (yet)".into());
                    };
                    crate::fonts::pick(which, Some(f), cx);
                    return Some("ok".into());
                }
                let Some((page, _)) = self.settings.as_ref() else { return Some("no settings".into()) };
                let page = page.downgrade();
                crate::settings_fonts::open_for_test(&page, which, rest, window, cx);
                "ok".into()
            }
            "inset" => {
                // Tests: `inset on|off`: the side lists' pressed-in text.
                let on = arg != "off";
                crate::settings_window::change(cx, |s| s.prefs.inset_text = on);
                crate::theme::apply(crate::theme::wants_dark(cx), cx);
                "ok".into()
            }
            "sidebar" => {
                // Tests: `sidebar <hex>` tries a light sidebar tone; `sidebar off` its own.
                crate::theme::try_sidebar_tone(u32::from_str_radix(arg.trim_start_matches('#'), 16).ok(), cx);
                "ok".into()
            }
            "fonts" => {
                // Tests: `fonts <app>|<chat>|<terminal>` (empty: the default).
                let mut parts = arg.split('|').map(|p| p.trim().to_string());
                let (ui, chat, term) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
                crate::settings_window::change(cx, |s| {
                    s.ui_font = ui;
                    s.chat_font = chat;
                    s.terminal_font = term;
                });
                crate::fonts::apply(cx);
                "ok".into()
            }
            "new-scripted" => {
                // A session on the scripted test backend (needs WUNDUR_TEST_BACKENDS=1).
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
            "fresh-start" => {
                // Tests: Start fresh from a summary, for the shown session.
                match self.active_workspace() {
                    Some(ws) => {
                        self.fresh_from_summary(ws, window, cx);
                        "ok".into()
                    }
                    None => "no session".into(),
                }
            }
            "project-add" => {
                // Tests: make a folder a project.
                add_project(PathBuf::from(arg), cx);
                "ok".into()
            }
            "project-ask" => {
                // Tests: the New project dialog for a folder.
                self.ask_new_project(PathBuf::from(arg), window, cx);
                "ok".into()
            }
            "project-rename" => {
                // Tests: the Rename project dialog for a project's folder.
                self.ask_project_name(PathBuf::from(arg), folder_name(Path::new(arg)), true, window, cx);
                "ok".into()
            }
            "project-name" => {
                // Tests: type into the project dialog.
                if let Some(f) = &self.project_form {
                    let input = f.input.clone();
                    input.update(cx, |i, cx| i.set_value(arg.to_string(), window, cx));
                }
                "ok".into()
            }
            "tools" => {
                // Tests: `tools` toggles the tool panels; `tools <name>` shows that one.
                match arg {
                    "" => self.toggle_tools(cx),
                    "Files" => self.show_tool("Files", cx),
                    "Changes" => self.show_tool("Changes", cx),
                    "Agent" => self.show_tool("Agent", cx),
                    _ => self.show_tool("Skills", cx),
                }
                format!("tools={} tab={}", Self::tools_shown(cx), self.tools_tab)
            }
            "active" => {
                // Tests: the shown session.
                format!("active={} ({}) browser={}", self.name(self.active, cx), self.active, self.browser.is_some())
            }
            "project-new-session" => {
                // Tests: New session from a project's menu (`arg`: its folder).
                self.new_session_for_project(PathBuf::from(arg), window, cx);
                format!("browser={}", self.browser.is_some())
            }
            "inbox" => {
                // Tests: open or close the inbox.
                self.inbox_open = !self.inbox_open;
                cx.notify();
                format!("inbox={} unread={}", self.inbox_open, crate::inbox::unread(cx))
            }
            "notify" => {
                // Tests: a notice from the shown session, as its agent's notify tool sends one:
                // `notify kind|title|message`.
                let mut parts = arg.splitn(3, '|');
                let kind = crate::inbox::Kind::parse(parts.next().unwrap_or_default());
                let (title, body) = (parts.next().unwrap_or("A notice").to_string(), parts.next().unwrap_or_default().to_string());
                let (number, from) = (self.tabs[self.active].number, self.name(self.active, cx));
                let choices = if kind == crate::inbox::Kind::NeedsYou { vec!["Yes".to_string(), "No".to_string()] } else { Vec::new() };
                crate::inbox::add(number, from, title, body, kind, choices, cx);
                format!("unread={}", crate::inbox::unread(cx))
            }
            "switch" => {
                // Tests: Go to session, typing `arg`.
                self.open_switcher(window, cx);
                if let Some(sw) = &self.switcher {
                    let input = sw.input.clone();
                    input.update(cx, |i, cx| i.set_value(arg.to_string(), window, cx));
                }
                "ok".into()
            }
            "switch-go" => {
                self.switch_to_picked(window, cx);
                format!("active={}", self.name(self.active, cx))
            }
            "project-create" => {
                self.create_project(cx);
                "ok".into()
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
            "pack-unfold" => {
                // Tests: unfold a part of a pack's card in Settings > Tools (`wundur/game-assets/svc/images`).
                match &self.settings {
                    Some((page, _)) => {
                        page.update(cx, |p, cx| {
                            p.packs.unfold(arg);
                            cx.notify();
                        });
                        "ok".into()
                    }
                    None => "settings are not open".into(),
                }
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

    /// A session's row in the list: its dot, name, folder, what it's doing; rename and close on
    /// hover; drag to reorder.
    /// A session's row in the full list; `project`, the root of the project it is listed under.
    fn session_row(&self, ix: usize, project: Option<&Path>, cx: &mut Context<Self>) -> AnyElement {
        // Two lines: its name, with how long ago it last did something (or how full its context
        // is, once that matters) at the right; then what it is about: that it needs the person
        // or is working, and the last turn's headline or what it is doing. Under a project it
        // sits in line with the project's name; the shown one is tinted, with an accent bar.
        let theme = cx.theme();
        let (fg, muted, border, hover, ring) = (crate::theme::list_text(cx), theme.muted_foreground, theme.border, theme.secondary_hover, theme.ring);
        let (yellow, red, blue, green) = (theme.yellow, theme.red, theme.blue, theme.green);
        let active = ix == self.active && self.browser.is_none();
        let number = self.tabs[ix].number;
        let row = &self.tabs[ix].row;
        let sel = ring.opacity(if theme.is_dark() { 0.18 } else { 0.10 });
        // At the right: the context once it fills (amber, then red), else how long ago.
        let filling = row.spend(cx).and_then(|sp| match sp {
            Spend::Context { fill: Fill::Filling, .. } => Some((sp.short(), theme.warning)),
            Spend::Context { fill: Fill::Nearly, .. } => Some((sp.short(), theme.danger)),
            _ => None,
        });
        let corner = filling.or_else(|| self.tabs[ix].changed.map(|t| (ago(t), muted)));
        // The line under it.
        let summary = row.summary.clone().filter(|_| crate::summaries_on(cx));
        let deeper = project.and_then(|root| {
            let key = project_root_of(row);
            key.strip_prefix(root).ok().filter(|r| !r.as_os_str().is_empty()).map(|r| r.display().to_string())
        });
        let (pill, about): (Option<(&'static str, Hsla)>, Option<String>) = match (row.dot, &summary) {
            (Some(Dot::Permission | Dot::Asking), _) => (Some(("Needs you", yellow)), row.activity.clone()),
            (Some(Dot::Working), _) => (Some(("Working", green)), row.activity.clone()),
            (Some(Dot::Agents), _) => (Some(("Agents working", green)), row.activity.clone()),
            (None, Some((st, needs_you, headline))) => {
                use wundur_core::summary::Status;
                let pill = match st {
                    Status::Done if *needs_you => Some(("Needs you", yellow)),
                    Status::Waiting => Some((st.label(), yellow)),
                    Status::Failed => Some((st.label(), red)),
                    Status::Partial => Some((st.label(), blue)),
                    Status::Done => None,
                };
                (pill, Some(headline.clone()))
            }
            (None, None) => (None, row.activity.clone()),
        };
        // Nothing said yet: where it is (a folder inside the project, else its branch).
        let about = about.or(deeper).or_else(|| if project.is_some() { row.branch.clone() } else { Some(row.place.clone()) });
        let worktree = row.worktree.then_some(());
        let dot = self.dot(ix, cx);
        let key = self.key(ix);
        let auto = self.list_name(ix, cx);
        let current = self.name(ix, cx);
        let title = panels::session_title(&key, auto.clone(), cx);
        let drag = SessionDrag { number, name: current.clone().into() };
        let (k1, a1, c1) = (key, auto, current);
        let menu = self.session_menu(ix, cx);
        div()
                .id(("session-row", number))
                .group(SharedString::from(format!("session-row-{number}")))
                .relative()
                .mx(px(6.))
                // In line with the project's name (past its chevron and folder).
                .pl(px(if project.is_some() { 22. } else { 8. }))
                .pr_2()
                .py(px(5.))
                .flex()
                .gap_2()
                .rounded(px(6.))
                .cursor_pointer()
                .when(active, |d| d.bg(sel))
                .when(!active, |d| d.hover(|d| d.bg(hover)))
                // The shown one's accent bar.
                .when(active, |d| d.child(div().absolute().left_0().top(px(4.)).bottom(px(4.)).w(px(3.)).rounded_full().bg(ring)))
                .child(
                    div()
                        .pt(px(5.))
                        .flex_none()
                        .w(px(10.))
                        .flex()
                        .justify_center()
                        .child(match dot {
                            Some((c, _)) => div().size(px(8.)).rounded_full().bg(c),
                            None => div().size(px(8.)).rounded_full().border_1().border_color(if active { ring } else { muted.opacity(0.6) }),
                        }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .text_color(fg)
                                        .when(active, |d| d.font_weight(FontWeight::SEMIBOLD))
                                        .child(title),
                                )
                                .children(corner.map(|(t, color)| div().flex_none().text_size(px(11.)).text_color(color).child(t))),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .text_size(px(11.5))
                                .text_color(muted)
                                .children(pill.map(|(word, color)| {
                                    div()
                                        .flex_none()
                                        .px(px(6.))
                                        .py(px(1.))
                                        .rounded(px(5.))
                                        .bg(color.opacity(0.16))
                                        .text_size(px(10.5))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(color)
                                        .child(word)
                                }))
                                .children(about.map(|a| crate::theme::pressed(div().min_w_0().truncate(), a, cx)))
                                .children(worktree.map(|_| div().flex_none().px_1().rounded_sm().bg(border).text_size(px(10.)).child("worktree"))),
                        ),
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
                        .bg(crate::theme::tool_ground(cx))
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
                .context_menu(menu)
                .into_any_element()
    }

    /// Go to session (Cmd+K): a field over the window listing the sessions whose name or
    /// project matches what is typed.
    pub fn open_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Go to a session or project…"));
        input.update(cx, |i, cx| i.focus(window, cx));
        let sub = cx.subscribe_in(&input, window, |s, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { .. } => s.switch_to_picked(window, cx),
            InputEvent::Change => {
                if let Some(sw) = s.switcher.as_mut() {
                    sw.picked = 0;
                }
                cx.notify();
            }
            _ => {}
        });
        self.switcher = Some(Switcher { input, picked: 0, _sub: sub });
        cx.notify();
    }

    /// The sessions matching what is typed in Go to session, in the list's order, each with
    /// its project's name. Every word typed must be in the session's name or its project's.
    fn switcher_matches(&self, cx: &App) -> Vec<(usize, String)> {
        let Some(sw) = &self.switcher else { return Vec::new() };
        let query = sw.input.read(cx).value().to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        let (projects, groups) = self.groups(cx);
        let mut found = Vec::new();
        for (group, members) in groups {
            let project = group.map(|pi| panels::tab_label(&project_key(&projects[pi].root), projects[pi].name.clone(), cx)).unwrap_or_default();
            for ix in members {
                let hay = format!("{} {}", self.name(ix, cx), project).to_lowercase();
                if words.iter().all(|w| hay.contains(w)) {
                    found.push((ix, project.clone()));
                }
            }
        }
        found
    }

    fn switch_to_picked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = self.switcher.as_ref().map_or(0, |s| s.picked);
        let target = self.switcher_matches(cx).get(picked).map(|(ix, _)| self.tabs[*ix].number);
        self.switcher = None;
        if let Some(ix) = target.and_then(|n| self.tabs.iter().position(|t| t.number == n)) {
            self.activate(ix, window, cx);
        }
        cx.notify();
    }

    fn render_switcher(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sw = self.switcher.as_ref()?;
        let matches = self.switcher_matches(cx);
        let theme = cx.theme();
        let (fg, muted, border, bg, sel) = (theme.foreground, theme.muted_foreground, theme.border, theme.background, theme.secondary);
        let picked = sw.picked.min(matches.len().saturating_sub(1));
        let mut list = div().id("switcher-list").flex().flex_col().max_h(px(360.)).overflow_y_scroll();
        for (i, (ix, project)) in matches.iter().enumerate() {
            let number = self.tabs[*ix].number;
            let status = self.tabs[*ix].row.dot.map(|d| match d {
                Dot::Permission | Dot::Asking => "Needs you",
                Dot::Working => "Working",
                Dot::Agents => "Agents working",
            });
            let detail = [Some(project.clone()).filter(|p| !p.is_empty()), status.map(String::from)].into_iter().flatten().collect::<Vec<_>>().join(" · ");
            list = list.child(
                div()
                    .id(("switcher-row", number))
                    .px_2()
                    .py(px(5.))
                    .rounded_md()
                    .flex()
                    .flex_col()
                    .cursor_pointer()
                    .when(i == picked, |d| d.bg(sel))
                    .child(div().truncate().text_sm().text_color(fg).child(self.name(*ix, cx)))
                    .child(div().truncate().text_xs().text_color(muted).child(detail))
                    .on_click(cx.listener(move |s, _, window, cx| {
                        s.switcher = None;
                        if let Some(ix) = s.tabs.iter().position(|t| t.number == number) {
                            s.activate(ix, window, cx);
                        }
                        cx.notify();
                    })),
            );
        }
        let count = matches.len();
        Some(
            div()
                .id("switcher")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(90.))
                .bg(gpui_kit::black().opacity(0.25))
                .occlude()
                .on_mouse_down(MouseButton::Left, cx.listener(|s, _, _, cx| {
                    s.switcher = None;
                    cx.notify();
                }))
                .capture_key_down(cx.listener(move |s, ev: &KeyDownEvent, _, cx| {
                    let step: isize = match ev.keystroke.key.as_str() {
                        "escape" => {
                            s.switcher = None;
                            cx.stop_propagation();
                            cx.notify();
                            return;
                        }
                        "down" => 1,
                        "up" => -1,
                        _ => return,
                    };
                    if let Some(sw) = s.switcher.as_mut()
                        && count > 0
                    {
                        sw.picked = (sw.picked as isize + step).rem_euclid(count as isize) as usize;
                    }
                    cx.stop_propagation();
                    cx.notify();
                }))
                .child(
                    div()
                        .id("switcher-box")
                        .w(px(480.))
                        .p_2()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .rounded_lg()
                        .border_1()
                        .border_color(border)
                        .bg(bg)
                        .shadow_lg()
                        // A press inside is not a press outside (which closes it).
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(Input::new(&sw.input).small())
                        .child(if count == 0 { div().p_2().text_xs().text_color(muted).child("No session matches.").into_any_element() } else { list.into_any_element() }),
                )
                .into_any_element(),
        )
    }

    /// The left column's splitter was let go: keep the width.
    fn end_list_resize(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.list_resizing) {
            let _ = cx.global::<AppSettings>().0.borrow().save();
            cx.notify();
        }
    }

    /// Whether the tool panels show under the sessions (Cmd+B hides them).
    fn tools_shown(cx: &App) -> bool {
        !cx.global::<AppSettings>().0.borrow().layout.sidebar_folded
    }

    /// Show the tool panels under the sessions, or hide them (Cmd+B).
    fn toggle_tools(&mut self, cx: &mut Context<Self>) {
        let settings = cx.global::<AppSettings>().0.clone();
        let folded = !settings.borrow().layout.sidebar_folded;
        settings.borrow_mut().layout.sidebar_folded = folded;
        let _ = settings.borrow().save();
        cx.notify();
    }

    /// Show tool panel `name` under the sessions (unfolding them).
    fn show_tool(&mut self, name: &'static str, cx: &mut Context<Self>) {
        self.tools_tab = name;
        if !Self::tools_shown(cx) {
            self.toggle_tools(cx);
        }
        cx.notify();
    }

    /// The shown session's tool panels under the sessions: a row of tabs, then the one picked.
    fn render_tools(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use crate::sheet_tabs::{Joined, Palette, simple_strip};
        let ws = self.active_workspace()?;
        let tools = ws.read(cx).tools(cx);
        let picked = tools.iter().position(|(n, _)| *n == self.tools_tab).unwrap_or(0);
        let theme = cx.theme();
        // As a tool panel in a session's layout looked: its light grey, a title bar that sits
        // slightly in, and sheet tabs hanging from the panel along the bottom.
        let (ground, content, border) = (theme.sidebar, crate::theme::tool_ground(cx), theme.border);
        let palette = Palette::new(ground, theme.foreground, content, theme.foreground, border, theme.ring);
        let title = self.tools_title(self.tools_tab_name(&tools, picked), true, cx);
        let labels: Vec<SharedString> = tools.iter().map(|(n, _)| SharedString::from(*n)).collect();
        let names: Vec<&'static str> = tools.iter().map(|(n, _)| *n).collect();
        let me = cx.entity().downgrade();
        let select: crate::sheet_tabs::OnSelect = Rc::new(move |ix, _, cx| {
            let name = names[ix];
            let _ = me.update(cx, |s, cx| {
                s.tools_tab = name;
                cx.notify();
            });
        });
        let tabs = simple_strip(&self.tools_strip, "tool-tab", &labels, picked, Joined::Top, palette, crate::dock_skin::BOTTOM_STRIP, None, select);
        let on_files = tools.get(picked).is_some_and(|(n, _)| *n == "Files");
        let view = tools.into_iter().nth(picked).map(|(_, v)| v)?;
        // Over the file tree: the folder it shows and its branch.
        let root_line = on_files.then(|| {
            let row = &self.tabs[self.active].row;
            let muted = cx.theme().muted_foreground;
            div()
                .flex_none()
                .px(px(12.))
                .pt(px(6.))
                .pb(px(4.))
                .flex()
                .items_center()
                .gap_2()
                .child(Icon::new(gpui_kit::assets::IconName::Folder).small().text_color(muted))
                .child(div().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(cx.theme().foreground).child(row.folder.clone()))
                .children(row.branch.clone().map(|b| {
                    div()
                        .flex_none()
                        .px(px(6.))
                        .py(px(1.))
                        .flex()
                        .items_center()
                        .gap_1()
                        .rounded(px(5.))
                        .bg(cx.theme().ring.opacity(0.12))
                        .text_size(px(11.))
                        .text_color(cx.theme().ring)
                        .child(Icon::new(gpui_kit::assets::IconName::GitBranch).xsmall())
                        .child(b)
                }))
        });
        Some(
            div()
                .size_full()
                .flex()
                .flex_col()
                .bg(content)
                .child(title)
                .children(root_line)
                .child(div().flex_1().min_h_0().child(view))
                .child(div().flex_none().h(px(crate::dock_skin::STRIP_GAP_ABOVE)))
                .child(tabs)
                .child(div().flex_none().h(px(crate::dock_skin::STRIP_GAP_BELOW)).bg(ground))
                .into_any_element(),
        )
    }

    /// The tool panels' title bar: the panel's name, and a button that hides them (⌄) or, while
    /// hidden, shows them again (⌃; the whole bar does too).
    fn tools_title(&self, name: &'static str, open: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        // A heading as the list's ("Projects"): the same size and weight on the column's own
        // ground, under a clear line between the two.
        let (ground, hover, fg) = (crate::theme::tool_ground(cx), theme.secondary_hover, crate::theme::list_text(cx));
        let line = crate::sheet_tabs::mix(theme.border, theme.foreground, 0.18);
        div()
            .id("tools-title")
            .flex_none()
            .h(px(40.))
            .px(px(14.))
            .flex()
            .items_center()
            .gap_2()
            .bg(ground)
            .border_t_1()
            .border_color(line)
            // As a project's row folds: the chevron at the left, the whole heading a click.
            .cursor_pointer()
            .hover(|d| d.bg(hover))
            .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(if open { "Hide the panels (Cmd+B)" } else { "Show the panels (Cmd+B)" }).build(w, cx))
            .on_click(cx.listener(|s, _, _, cx| s.toggle_tools(cx)))
            .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).small().text_color(cx.theme().muted_foreground))
            .child(div().flex_1().min_w_0().truncate().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(fg).child(name))
            .into_any_element()
    }

    /// The tool panels while hidden: their title bar alone, under the list.
    fn render_tools_tabs(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ws = self.active_workspace()?;
        let tools = ws.read(cx).tools(cx);
        let picked = tools.iter().position(|(n, _)| *n == self.tools_tab).unwrap_or(0);
        let name = self.tools_tab_name(&tools, picked);
        Some(self.tools_title(name, false, cx))
    }

    fn tools_tab_name(&self, tools: &[(&'static str, AnyView)], picked: usize) -> &'static str {
        tools.get(picked).map_or(self.tools_tab, |(n, _)| *n)
    }

    /// Ask for a folder, then for the project's name.
    fn new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Make project".into()) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(root) = paths.into_iter().next() else { return };
            let _ = this.update_in(cx, |s, window, cx| s.ask_new_project(root, window, cx));
        })
        .detach();
    }

    /// Ask what to call the project made of `root` (its folder's name to start with).
    pub fn ask_new_project(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = folder_name(&root);
        self.ask_project_name(root, name, false, window, cx);
    }

    /// The project dialog: a new project's name, or (`renaming`) a new name for one.
    fn ask_project_name(&mut self, root: PathBuf, name: String, renaming: bool, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        input.update(cx, |i, cx| {
            i.focus(window, cx);
            i.select_all(window, cx);
        });
        let sub = cx.subscribe_in(&input, window, |s, _, ev: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                s.create_project(cx);
            }
        });
        self.project_form = Some(ProjectForm { root, input, renaming, _sub: sub });
        cx.notify();
    }

    /// Make the asked-about folder a project under the name typed (its folder's name if none).
    fn create_project(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.project_form.take() else { return };
        let typed = f.input.read(cx).value().trim().to_string();
        let name = if typed.is_empty() { folder_name(&f.root) } else { typed };
        // Named here, not by a rename left over from an earlier project of the same folder.
        let _ = cx.global::<panels::Names>().0.borrow_mut().set(&project_key(&f.root), "");
        name_project(f.root, name, cx);
        cx.notify();
    }

    fn render_project_form(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let f = self.project_form.as_ref()?;
        let theme = cx.theme();
        let (fg, muted, border, bg) = (theme.foreground, theme.muted_foreground, theme.border, theme.background);
        Some(
            div()
                .id("project-form")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(90.))
                .bg(gpui_kit::black().opacity(0.25))
                .occlude()
                .capture_key_down(cx.listener(|s, ev: &KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        s.project_form = None;
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
                        .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(if f.renaming { "Rename project" } else { "New project" }))
                        .child(div().text_xs().text_color(muted).child(f.root.display().to_string()))
                        .child(Input::new(&f.input).small())
                        .child(
                            div()
                                .text_xs()
                                .line_height(rems(1.4))
                                .text_color(muted)
                                .child("Its sessions (in this folder or inside it) are listed under this name."),
                        )
                        .child(
                            div()
                                .pt_1()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(Button::new("project-form-cancel").small().ghost().label("Cancel").on_click(cx.listener(|s, _, _, cx| {
                                    s.project_form = None;
                                    cx.notify();
                                })))
                                .child(Button::new("project-form-go").small().primary().label(if f.renaming { "Rename" } else { "Create" }).on_click(cx.listener(|s, _, _, cx| s.create_project(cx)))),
                        ),
                )
                .into_any_element(),
        )
    }

    /// A project's row: fold, its name (renamed like a session), what its sessions are doing,
    /// New session in its folder, and a menu.
    fn project_header(&self, pi: usize, project: &wundur_core::settings::Project, members: &[usize], cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, hover, yellow, green) = (crate::theme::list_text(cx), theme.muted_foreground, theme.secondary_hover, theme.yellow, theme.green);
        let waiting = members.iter().filter(|ix| matches!(self.tabs[**ix].row.dot, Some(Dot::Permission | Dot::Asking))).count();
        let working = members.iter().filter(|ix| matches!(self.tabs[**ix].row.dot, Some(Dot::Working | Dot::Agents))).count();
        let key = project_key(&project.root);
        let title = panels::session_title(&key, project.name.clone(), cx);
        let (root, root2, root3, k1, a1) = (project.root.clone(), project.root.clone(), project.root.clone(), key.clone(), project.name.clone());
        let weak_shell = cx.entity().downgrade();
        let folded = project.folded;
        div()
            .id(("project-row", pi))
            .group(SharedString::from(format!("project-row-{pi}")))
            .mx(px(6.))
            .mt_2()
            .px_1()
            .h(px(28.))
            .flex()
            .items_center()
            .gap_1()
            .rounded(px(8.))
            .cursor_pointer()
            .hover(|d| d.bg(hover))
            .child(Icon::new(if folded { IconName::ChevronRight } else { IconName::ChevronDown }).xsmall().text_color(muted))
            .child(Icon::new(gpui_kit::assets::IconName::Folder).small().text_color(muted))
            .child(div().flex_1().min_w_0().truncate().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(title))
            .when(waiting > 0, |d| d.child(div().flex_none().text_size(px(10.5)).text_color(yellow).child(format!("{waiting} waiting"))))
            .when(working > 0, |d| d.child(div().flex_none().text_size(px(10.5)).text_color(green).child(format!("{working} working"))))
            .when(waiting == 0 && working == 0, |d| d.child(div().flex_none().text_size(px(10.5)).text_color(muted).child(members.len().to_string())))
            .when(!is_remote_project(&project.root), |d| d.child(
                Button::new(("project-session", pi))
                    .icon(IconName::Plus)
                    .xsmall()
                    .ghost()
                    .tooltip("New session in this project")
                    .on_click(cx.listener(move |s, _, window, cx| {
                        cx.stop_propagation();
                        s.open_browser(Arc::new(LocalHost), root.clone(), None, window, cx);
                    })),
            ))
            .on_click(move |_, _, cx| Self::toggle_project(root2.clone(), cx))
            .context_menu(move |menu, _, cx| {
                let (k, a, r) = (k1.clone(), a1.clone(), root3.clone());
                let saved = cx.global::<AppSettings>().0.borrow().projects.iter().any(|p| p.root == r);
                let (w, r_new) = (weak_shell.clone(), r.clone());
                menu.item(PopupMenuItem::new("New session…").on_click(move |_, window, cx| {
                    let _ = w.update(cx, |s, cx| s.new_session_for_project(r_new.clone(), window, cx));
                }))
                .separator()
                .item(PopupMenuItem::new("Rename").on_click(move |_, window, cx| start_tab_rename(k.clone(), a.clone(), a.clone(), window, cx)))
                    .when(saved, |m| {
                        m.item(PopupMenuItem::new("Forget project (its sessions stay)").on_click(move |_, _, cx| {
                            let r = r.clone();
                            crate::settings_window::change(cx, move |s| s.projects.retain(|p| p.root != r));
                        }))
                    })
            })
            .into_any_element()
    }

    /// The sessions as the lists show them, by project: the projects kept in the settings
    /// (named, folded, or with no session open), then one for each other folder with sessions
    /// (a folder inside another such folder goes under it), and one per folder on each other
    /// machine. Every session is in exactly one, so there is no Other.
    fn groups(&self, cx: &App) -> (Vec<wundur_core::settings::Project>, Vec<Group>) {
        let mut projects = cx.global::<AppSettings>().0.borrow().projects.clone();
        let saved = projects.len();
        // Each session's key: its folder, or `ssh://host/folder` on another machine.
        let keys: Vec<PathBuf> = self.tabs.iter().map(|t| project_root_of(&t.row)).collect();
        let mut homes: Vec<Option<usize>> =
            keys.iter().map(|k| wundur_core::settings::project_of(&projects, k).and_then(|p| projects.iter().position(|q| q.root == p.root))).collect();
        // The other folders: the outermost ones become projects, in the order their first
        // session is in.
        let loose: Vec<&PathBuf> = keys.iter().zip(&homes).filter(|(_, h)| h.is_none()).map(|(k, _)| k).collect();
        for ix in 0..self.tabs.len() {
            if homes[ix].is_some() {
                continue;
            }
            let key = &keys[ix];
            let outer = loose.iter().filter(|o| key.starts_with(o)).min_by_key(|o| o.components().count()).map(|o| (*o).clone()).unwrap_or_else(|| key.clone());
            let pi = match projects.iter().skip(saved).position(|p| p.root == outer) {
                Some(i) => saved + i,
                None => {
                    projects.push(wundur_core::settings::Project { name: project_default_name(&outer), root: outer, folded: false });
                    projects.len() - 1
                }
            };
            homes[ix] = Some(pi);
        }
        let groups = (0..projects.len()).map(|pi| (Some(pi), (0..self.tabs.len()).filter(|ix| homes[*ix] == Some(pi)).collect())).collect();
        (projects, groups)
    }

    /// Fold or unfold the project at `root`.
    fn toggle_project(root: PathBuf, cx: &mut App) {
        crate::settings_window::change(cx, move |s| match s.projects.iter_mut().find(|p| p.root == root) {
            Some(p) => p.folded = !p.folded,
            // One made of a folder's sessions: kept from now on, folded.
            None => s.projects.push(wundur_core::settings::Project { name: project_default_name(&root), root, folded: true }),
        });
    }

    /// What waits on the person in every session, for the inbox: each open question or
    /// permission request, with its session's place in the list.
    fn waiting(&self, cx: &App) -> Vec<(usize, wundur_core::conversation::PermissionPrompt)> {
        let mut found = Vec::new();
        for (ix, t) in self.tabs.iter().enumerate() {
            let session = t.ws.read(cx).session.read(cx);
            for p in session.chat.pending_permissions().filter(|p| p.answer.is_none()) {
                found.push((ix, p.clone()));
            }
        }
        found
    }

    /// Notices unread plus what waits on the person: the toolbar's count.
    fn inbox_count(&self, cx: &App) -> usize {
        crate::inbox::unread(cx) + self.waiting(cx).len()
    }

    /// Answer notice `id` with `answer`: it goes to the session it came from as the person's
    /// next message (waiting its turn while the agent works).
    fn answer_notice(&mut self, id: u64, answer: String, cx: &mut Context<Self>) {
        let Some(n) = crate::inbox::get(id, cx) else { return };
        if answer.trim().is_empty() {
            return;
        }
        if let Some(t) = self.tabs.iter().find(|t| t.number == n.session) {
            let session = t.ws.read(cx).session.clone();
            let text = crate::inbox::reply_text(&n, &answer);
            session.update(cx, |s, cx| s.send(&text, cx));
            crate::inbox::answered(id, answer.trim().to_string(), cx);
        }
        self.inbox_reply = None;
        cx.notify();
    }

    /// A field to answer notice `id` in the person's own words (Enter sends).
    fn open_notice_reply(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Your answer…"));
        input.update(cx, |i, cx| i.focus(window, cx));
        let sub = cx.subscribe_in(&input, window, move |s, input, ev: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                let text = input.read(cx).value().to_string();
                s.answer_notice(id, text, cx);
            }
        });
        self.inbox_reply = Some((id, input, sub));
        cx.notify();
    }

    /// The toolbar along the window's left edge: Projects (shows or hides the list), Inbox (the
    /// agents' notices, with how many are unread), and Settings at the bottom.
    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, fg, hover, sel, accent, bg) = (theme.muted_foreground, theme.foreground, theme.secondary_hover, theme.secondary, theme.ring, theme.sidebar);
        let unread = self.inbox_count(cx);
        let item = |id: &'static str, icon: Icon, tip: &'static str, on: bool| {
            div()
                .id(id)
                .relative()
                .size(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .cursor_pointer()
                .text_color(if on { fg } else { muted })
                .when(on, |d| d.bg(sel))
                .when(!on, |d| d.hover(|d| d.bg(hover)))
                .child(icon)
                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip).build(w, cx))
        };
        div()
            .id("toolbar")
            .w(px(TOOLBAR_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .pt_1()
            .pb_2()
            .bg(bg)
            .child(
                item("tb-projects", Icon::new(gpui_kit::assets::IconName::Folder), "Projects (Cmd+Shift+B)", !self.folded && !self.tabs.is_empty() && self.settings.is_none())
                    // From Settings: back to the sessions, with the list shown.
                    .on_click(cx.listener(|s, _, window, cx| {
                        if s.settings.is_some() {
                            s.close_settings(window, cx);
                            if s.folded {
                                s.toggle_folded(cx);
                            }
                        } else {
                            s.toggle_folded(cx);
                        }
                    })),
            )
            .child(
                item("tb-inbox", Icon::new(gpui_kit::assets::IconName::Inbox), "Inbox: notices from your sessions' agents", self.inbox_open)
                    .when(unread > 0, |d| {
                        d.child(
                            div()
                                .absolute()
                                .top(px(1.))
                                .right(px(0.))
                                .min_w(px(15.))
                                .h(px(15.))
                                .px(px(3.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .bg(accent)
                                .text_size(px(9.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(gpui_kit::white())
                                .child(if unread > 99 { "99+".to_string() } else { unread.to_string() }),
                        )
                    })
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.inbox_open = !s.inbox_open;
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .child(
                item("tb-settings", Icon::new(IconName::Settings), "Settings (Cmd+,)", self.settings.is_some()).on_click(cx.listener(|s, _, window, cx| {
                    if s.settings.is_some() {
                        s.close_settings(window, cx)
                    } else {
                        s.show_settings(window, cx)
                    }
                })),
            )
            .into_any_element()
    }

    /// The inbox, over the content beside the toolbar: the newest notice first, each with who
    /// sent it and when; one opens its session. Clicking outside closes it.
    fn render_inbox(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, bg, hover) = (theme.foreground, theme.muted_foreground, theme.border, theme.background, theme.secondary_hover);
        let (green, yellow, red, blue) = (theme.green, theme.yellow, theme.red, theme.blue);
        let items = crate::inbox::items(cx);
        let waiting = self.waiting(cx);
        let any_waiting = !waiting.is_empty();
        let mut list = div().id("inbox-list").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().px_2().pb_2();
        // What waits on the person now, in any session: answered here, or opened.
        if !waiting.is_empty() {
            list = list.child(div().px_2().pt_1().pb_1().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(yellow).child("Waiting on you"));
        }
        for (ix, p) in waiting {
            let number = self.tabs[ix].number;
            let name = self.name(ix, cx);
            let session = self.tabs[ix].ws.read(cx).session.clone();
            let questions = wundur_core::questions::parse(&p.tool_name, &p.input);
            let open = cx.listener(move |s, _, window, cx| {
                cx.stop_propagation();
                s.inbox_open = false;
                if let Some(ix) = s.tabs.iter().position(|t| t.number == number) {
                    s.activate(ix, window, cx);
                }
                cx.notify();
            });
            let mut row = div().px_2().py(px(8.)).flex().flex_col().gap_1().rounded(px(8.)).bg(hover.opacity(0.5));
            match questions.as_deref() {
                // One question, one answer: its choices as buttons.
                Some([q]) if !q.multi && !q.options.is_empty() => {
                    let mut choices = div().flex().flex_wrap().gap_1();
                    for (i, (label, _)) in q.options.iter().enumerate() {
                        let (session, rid, question, label2) = (session.clone(), p.request_id.clone(), q.question.clone(), label.clone());
                        choices = choices.child(
                            Button::new(SharedString::from(format!("wait-{}-{i}", p.request_id))).xsmall().label(label.clone()).on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                session.update(cx, |s, cx| s.answer_question(&rid, vec![(question.clone(), vec![label2.clone()])], cx));
                            }),
                        );
                    }
                    row = row
                        .child(div().text_sm().text_color(fg).child(q.question.clone()))
                        .child(choices)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(div().flex_1().text_size(px(10.5)).text_color(muted).child(format!("{name} asks")))
                                .child(Button::new(SharedString::from(format!("wait-open-{}", p.request_id))).xsmall().ghost().label("Open").on_click(open)),
                        );
                }
                // Several questions, or several answers: answered in the session's own card.
                Some(qs) => {
                    row = row
                        .child(div().text_sm().text_color(fg).child(qs.first().map(|q| q.question.clone()).unwrap_or_default()))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(div().flex_1().text_size(px(10.5)).text_color(muted).child(format!("{name} asks {} question{}", qs.len(), if qs.len() == 1 { "" } else { "s" })))
                                .child(Button::new(SharedString::from(format!("wait-open-{}", p.request_id))).xsmall().label("Answer in the session").on_click(open)),
                        );
                }
                // Permission to use a tool: shown, answered in the session (where all of it is).
                None => {
                    row = row
                        .child(div().text_sm().text_color(fg).child(format!("{}: {}", p.tool_name, p.summary)))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(div().flex_1().text_size(px(10.5)).text_color(muted).child(format!("{name} needs permission")))
                                .child(Button::new(SharedString::from(format!("wait-open-{}", p.request_id))).xsmall().label("Open").on_click(open)),
                        );
                }
            }
            list = list.child(row);
        }
        if !items.is_empty() && any_waiting {
            list = list.child(div().px_2().pt_3().pb_1().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(muted).child("Notices"));
        }
        if items.is_empty() && !any_waiting {
            list = list.child(
                div()
                    .p_3()
                    .text_sm()
                    .line_height(rems(1.4))
                    .text_color(muted)
                    .child("Nothing yet. Agents in any session can send you a notice here (Wundur's notify tool): a long task done, a decision waiting on you, something that went wrong."),
            );
        }
        for n in items {
            let (id, number) = (n.id, n.session);
            // Answering it: its choices, and the person's own words.
            let answer_row = if let Some(a) = &n.answer {
                Some(div().text_xs().text_color(muted).child(format!("You answered: {a}")).into_any_element())
            } else if n.asks() {
                let mut row = div().pt_1().flex().flex_wrap().gap_1();
                for (i, c) in n.choices.iter().enumerate() {
                    let c2 = c.clone();
                    row = row.child(Button::new(SharedString::from(format!("notice-{id}-{i}"))).xsmall().label(c.clone()).on_click(cx.listener(move |s, _, _, cx| {
                        cx.stop_propagation();
                        s.answer_notice(id, c2.clone(), cx);
                    })));
                }
                let replying = self.inbox_reply.as_ref().filter(|(rid, _, _)| *rid == id).map(|(_, input, _)| input.clone());
                Some(match replying {
                    Some(input) => div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(row)
                        .child(div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(Input::new(&input).small()))
                        .into_any_element(),
                    None => row
                        .child(Button::new(SharedString::from(format!("notice-{id}-reply"))).xsmall().ghost().label("Reply…").on_click(cx.listener(move |s, _, window, cx| {
                            cx.stop_propagation();
                            s.open_notice_reply(id, window, cx);
                        })))
                        .into_any_element(),
                })
            } else {
                None
            };
            let color = match n.kind {
                crate::inbox::Kind::Done => green,
                crate::inbox::Kind::NeedsYou => yellow,
                crate::inbox::Kind::Problem => red,
                crate::inbox::Kind::Info => blue,
            };
            list = list.child(
                div()
                    .id(("inbox-notice", id as usize))
                    .px_2()
                    .py(px(8.))
                    .flex()
                    .gap_2()
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(|d| d.bg(hover))
                    .child(div().pt(px(5.)).flex_none().child(div().size(px(8.)).rounded_full().when_else(n.read, |d| d.border_1().border_color(color), |d| d.bg(color))))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().text_sm().text_color(fg).when(!n.read, |d| d.font_weight(FontWeight::SEMIBOLD)).child(n.title.clone()))
                            .when(!n.body.is_empty(), |d| d.child(div().text_xs().line_height(rems(1.35)).text_color(muted).child(n.body.clone())))
                            .children(answer_row)
                            .child(div().text_size(px(10.5)).text_color(muted.opacity(0.8)).child(format!("{} · {}", n.from, ago(n.at)))),
                    )
                    .on_click(cx.listener(move |s, _, window, cx| {
                        crate::inbox::mark_read(id, cx);
                        s.inbox_open = false;
                        if let Some(ix) = s.tabs.iter().position(|t| t.number == number) {
                            s.activate(ix, window, cx);
                        }
                        cx.notify();
                    })),
            );
        }
        div()
            .id("inbox")
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(|s, _, _, cx| {
                s.inbox_open = false;
                cx.notify();
            }))
            .child(
                div()
                    .id("inbox-panel")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(TOOLBAR_WIDTH))
                    .w(px(INBOX_WIDTH))
                    .flex()
                    .flex_col()
                    .bg(bg)
                    .border_r_1()
                    .border_color(border)
                    .shadow_lg()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .flex_none()
                            .h(px(40.))
                            .px(px(14.))
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(div().flex_1().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).text_color(fg).child("Inbox"))
                            .child(Button::new("inbox-read").xsmall().ghost().label("Mark all read").on_click(|_, _, cx| crate::inbox::mark_all_read(cx)))
                            .child(Button::new("inbox-clear").xsmall().ghost().label("Clear").on_click(|_, _, cx| crate::inbox::clear(cx))),
                    )
                    .child(list),
            )
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, hover, ring, bg) = (crate::theme::list_text(cx), theme.muted_foreground, theme.secondary_hover, theme.ring, theme.sidebar);
        let mut col = div().id("session-list").flex_1().min_h_0().flex().flex_col().overflow_y_scroll();
        // With projects: each project's sessions under it (a session in its folder or inside
        // it), then the rest under Other. Without: the plain list.
        let (projects, groups) = self.groups(cx);
        for (group, members) in groups {
            match group {
                Some(pi) => {
                    let project = &projects[pi];
                    col = col.child(self.project_header(pi, project, &members, cx));
                    // Open, its sessions under it, in line with its name.
                    if !project.folded && !members.is_empty() {
                        let root = project.root.clone();
                        let mut under = div().pt(px(2.)).pb_1().flex().flex_col().gap(px(1.));
                        for ix in members {
                            under = under.child(self.session_row(ix, Some(&root), cx));
                        }
                        col = col.child(under);
                    }
                }
                None => {
                    if !projects.is_empty() {
                        col = col.child(div().px(px(14.)).pt_3().pb_1().text_size(px(11.)).text_color(muted).child("Other"));
                    }
                    for ix in members {
                        col = col.child(self.session_row(ix, None, cx));
                    }
                }
            }
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
        let width = cx.global::<AppSettings>().0.borrow().layout.list_width.clamp(LIST_MIN, LIST_MAX);
        div()
            .w(px(width))
            .flex_none()
            .h_full()
            .ml(px(6.))
            .relative()
            // Nothing in it reaches past its edges (to be seen or clicked).
            .overflow_hidden()
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
                    .child("Projects")
                    .child(div().flex_1())
                    .child(
                        Button::new("project-new")
                            .icon(gpui_kit::assets::IconName::FolderPlus)
                            .xsmall()
                            .ghost()
                            .tooltip("New project: a folder, its sessions grouped under it")
                            .on_click(cx.listener(|s, _, window, cx| s.new_project(window, cx))),
                    ),
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
            .child(match Self::tools_shown(cx).then(|| self.render_tools(cx)).flatten() {
                // Each half holds its own: the list scrolls inside its share and the panels keep
                // theirs (a list running on under the panels was hidden there but still took
                // clicks, and pushed the panels' tabs off the bottom).
                // The room left under the heading and New session, not the column's whole height.
                Some(tools) => div().flex_1().min_h_0().w_full().child(v_resizable("list-and-tools")
                    .with_handle_appearance(Rc::new(|handle, _, cx| Some(crate::dock_skin::split_line(handle, cx))))
                    .child(
                        resizable_panel()
                            .size_range(px(120.)..Pixels::MAX)
                            .child(div().size_full().min_h_0().overflow_hidden().flex().flex_col().child(col)),
                    )
                    .child(
                        resizable_panel()
                            .size(px(TOOLS_HEIGHT))
                            .size_range(px(140.)..Pixels::MAX)
                            .child(div().size_full().min_h_0().overflow_hidden().child(tools)),
                    ))
                    .into_any_element(),
                // Hidden, the panels leave their tabs along the bottom: one opens them on it.
                None => match self.render_tools_tabs(cx) {
                    Some(tabs) => div().flex_1().min_h_0().w_full().flex().flex_col().child(div().flex_1().min_h_0().flex().flex_col().child(col)).child(tabs).into_any_element(),
                    None => col.into_any_element(),
                },
            })
            .child(crate::dock_skin::corner_cap(true, true, bg))
            // Its right edge, the splitter: a thin line that thickens in the accent colour under
            // the pointer and while dragged. Drawn last, so it is on top of the list and the
            // panels, inside the column's edge.
            .child({
                let (line, accent, dragging) = (cx.theme().border, cx.theme().ring, self.list_resizing);
                div()
                    .id("list-resize")
                    .group("list-resize")
                    .absolute()
                    .top_0()
                    .right_0()
                    .w(px(8.))
                    .h_full()
                    .occlude()
                    .cursor_col_resize()
                    .on_drag(ListResize, |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.new(|_| ListResize)
                    })
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .right_0()
                            .w(px(if dragging { 3. } else { 1. }))
                            .bg(if dragging { accent } else { line })
                            .group_hover("list-resize", move |d| d.w(px(3.)).bg(accent)),
                    )
            })
            .into_any_element()
    }

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
    let out = wundur_core::platform::command("git").arg("-C").arg(folder).args(["rev-parse", "--show-toplevel"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let top = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let branch = wundur_core::host::LocalHost.git_branch(&top);
    Some((top, branch))
}

/// A free name for a worktree of the repository at `top`: `<repo>-2`, `<repo>-3`, ... (a
/// folder next to the repository and a `wundur/<name>` branch, neither existing yet).
pub fn suggest_worktree_name(top: &Path) -> String {
    let name = folder_name(top);
    let parent = top.parent().unwrap_or(top);
    (2..).map(|n| format!("{name}-{n}")).find(|c| !parent.join(c).exists() && !branch_exists(top, &format!("wundur/{c}"))).unwrap_or(name)
}

fn branch_exists(top: &Path, branch: &str) -> bool {
    wundur_core::platform::command("git")
        .arg("-C")
        .arg(top)
        .args(["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// `git worktree add -b wundur/<name> <parent of top>/<name>`: the folder and branch the
/// person saw and named. Refuses a name already taken.
fn make_worktree(top: &Path, name: &str) -> Result<(PathBuf, String), String> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return Err(format!("\"{name}\" is not a usable folder name"));
    }
    let parent = top.parent().ok_or("the repository has no parent folder")?;
    let path = parent.join(name);
    let branch = format!("wundur/{name}");
    if path.exists() {
        return Err(format!("{} exists already", path.display()));
    }
    if branch_exists(top, &branch) {
        return Err(format!("the branch {branch} exists already"));
    }
    let out = wundur_core::platform::command("git")
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
        // The splitter's drag is over once the window has none (it ends wherever the button
        // came up, whatever was under it): keep the width and drop the highlight.
        if self.list_resizing && !cx.has_active_drag() {
            self.end_list_resize(cx);
        }
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
                // Hide or show the projects list, where a Mac app keeps its sidebar's button.
                .child(
                    button("t-sessions", Icon::new(IconName::PanelLeft), "Projects list (Cmd+Shift+B)")
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
                            .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(fg).child("Wundur"))
                            // Parent mode shows, so the child knows.
                            .when(crate::parent_mode::on(cx), |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .px_1p5()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .text_color(fg)
                                        .child("Parent mode"),
                                )
                            })
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
                                button("t-split", Icon::new(IconName::PanelRight), "Split the shown file or terminal (Cmd+\\)")
                                    .on_click(to_ws(|w, win, cx| w.split_right(win, cx))),
                            )
                            .child(
                                button("t-reset", Icon::new(IconName::RotateCw), "Reset layout")
                                    .on_click(to_ws(|w, win, cx| w.reset_layout(win, cx))),
                            ),
                    )
                })
                .child(div().flex_none().pr_1().occlude().child(menu_button)),
        );
        let main = match (&self.settings, &self.browser) {
            (Some((page, _)), _) => page.clone().into_any_element(),
            (None, Some(b)) => b.clone().into_any_element(),
            (None, None) => match self.active_workspace() {
                Some(w) => w.into_any_element(),
                None => div().into_any_element(),
            },
        };
        // Hidden (the toolbar's Projects, or Cmd+Shift+S), the list goes entirely and the work
        // has the room.
        // Settings, while open, has the whole window beside the toolbar.
        let list = (!self.tabs.is_empty() && !self.folded && self.settings.is_none()).then(|| self.render_list(cx));
        let toolbar = self.render_toolbar(cx);
        let inbox = self.inbox_open.then(|| self.render_inbox(cx));
        let list_shown = list.is_some();
        let folder_rename = self.render_folder_rename(cx);
        let project_form = self.render_project_form(cx);
        let switcher = self.render_switcher(cx);
        // Web pages are native views over the window: they hide while an overlay is open.
        crate::web_panel::set_covered(
            self.folder_rename.is_some() || self.project_form.is_some() || self.switcher.is_some() || self.inbox_open || crate::image_view::is_open(cx)
                || crate::skill_installer::is_open(cx),
        );
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(bg)
            .text_color(fg)
            .font_family(crate::fonts::ui(cx))
            // A touch heavier than regular, as Codex sets SF Pro (430): denser, still not bold.
            .font_weight(crate::fonts::weight(&crate::fonts::ui(cx)))
            // An image opened full size: Esc closes it and Cmd+C copies it, wherever the
            // keyboard is.
            .capture_key_down(cx.listener(|_, ev: &KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" && crate::image_view::is_open(cx) {
                    crate::image_view::close(cx);
                    cx.stop_propagation();
                } else if ev.keystroke.key == "c" && ev.keystroke.modifiers.secondary() && crate::image_view::copy(cx) {
                    // Cmd+C with an image open copies the image.
                    cx.stop_propagation();
                } else if ev.keystroke.key == "escape" && crate::skill_installer::is_open(cx) {
                    crate::skill_installer::close(cx);
                    cx.stop_propagation();
                }
            }))
            .track_focus(&self.focus)
            .on_action(cx.listener(|s, _: &NewSession, w, cx| s.new_session(w, cx)))
            .on_action(cx.listener(|s, _: &SwitchSession, w, cx| s.open_switcher(w, cx)))
            .on_action(cx.listener(|s, _: &ToggleSidebar, _, cx| s.toggle_tools(cx)))
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
                    // Dragging the left column's splitter: the column follows the pointer; letting
                    // go anywhere ends it (a drag of the window's own, which ends wherever the
                    // button comes up), and the width is kept.
                    .on_drag_move(cx.listener(|s, ev: &DragMoveEvent<ListResize>, _, cx| {
                        let width = (f32::from(ev.event.position.x) - TOOLBAR_WIDTH - 6.).clamp(LIST_MIN, LIST_MAX);
                        cx.global::<AppSettings>().0.borrow_mut().layout.list_width = width;
                        s.list_resizing = true;
                        cx.notify();
                    }))
                    .on_drop(cx.listener(|s, _: &ListResize, _, cx| s.end_list_resize(cx)))
                    .when(self.list_resizing, |d| {
                        d.cursor_col_resize().on_mouse_up(MouseButton::Left, cx.listener(|s, _, _, cx| s.end_list_resize(cx)))
                    })
                    .child(toolbar)
                    .children(list)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .relative()
                            .child(main)
                            // With the list hidden, the work area is rounded at its top left
                            // beside the toolbar; beside the list, the list's corner is.
                            .when(!list_shown, |d| d.child(crate::dock_skin::corner_cap(true, true, bg))),
                    )
                    .children(inbox),
            )
            .children(folder_rename)
            .children(project_form)
            .children(switcher)
            .children(crate::skill_installer::render(cx))
            .children(crate::image_view::render(cx))
    }
}

/// A session closed for a folder rename, to open again: where, with what, and its id.
type Reopen = (Arc<dyn Host>, PathBuf, Arc<dyn Backend>, Option<String>);

/// A group of the session list: a project (by its index), or none (Other), and its sessions.
type Group = (Option<usize>, Vec<usize>);

/// The left column's splitter being dragged (nothing is drawn under the pointer).
struct ListResize;

impl Render for ListResize {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Go to session: the field to type in and which match is picked.
struct Switcher {
    input: Entity<InputState>,
    picked: usize,
    _sub: Subscription,
}

/// A folder about to be made a project, and the name asked for it.
struct ProjectForm {
    root: PathBuf,
    input: Entity<InputState>,
    /// Renaming a project, not making one.
    renaming: bool,
    _sub: Subscription,
}

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

/// Wundur > Settings… and Cmd+,: Settings in the window.
pub fn open_settings(cx: &mut App) {
    let Some((handle, shell)) = cx.try_global::<crate::control::ControlTargets>().and_then(|t| t.workspace.clone()) else { return };
    // After the key press or menu click that asked, which still holds the window.
    cx.defer(move |cx| {
        let _ = handle.update(cx, |_, window, cx| {
            let _ = shell.update(cx, |s, cx| s.show_settings(window, cx));
        });
    });
}

/// What shows while a conversation is being read: a few message shapes, pulsing.
pub fn loading_skeleton(cx: &App) -> AnyElement {
    let shade = cx.theme().muted_foreground.opacity(0.14);
    let bar = |w: f32, h: f32| div().w(relative(w)).h(px(h)).rounded(px(6.)).bg(shade);
    div()
        .size_full()
        .flex()
        .justify_center()
        .child(
            div()
                .w_full()
                .max_w(px(760.))
                .px_6()
                .pt_6()
                .flex()
                .flex_col()
                .gap_3()
                .child(div().flex().justify_end().child(bar(0.4, 34.)))
                .child(bar(0.92, 14.))
                .child(bar(0.86, 14.))
                .child(bar(0.6, 14.))
                .child(div().h(px(10.)))
                .child(div().flex().justify_end().child(bar(0.3, 34.)))
                .child(bar(0.9, 14.))
                .child(bar(0.74, 14.))
                .with_animation(
                    "restoring",
                    Animation::new(std::time::Duration::from_millis(1100)).repeat().with_easing(pulsating_between(0.45, 1.0)),
                    |d, t| d.opacity(t),
                ),
        )
        .into_any_element()
}

/// The name store's key for a project (its name, renamed in the list as a session's is).
fn project_key(root: &Path) -> String {
    format!("project:{}", root.display())
}

/// The project key of a session's folder: the folder, or `ssh://host/folder` for one on another
/// machine (so its projects never mix with this machine's).
fn project_root_of(row: &Row) -> PathBuf {
    match &row.host {
        Some(host) => PathBuf::from(format!("ssh://{host}{}", row.root.display())),
        None => row.root.clone(),
    }
}

/// When a session last wrote its transcript, as its agent can tell.
fn transcript_time(w: &Workspace, row: &Row, cx: &App) -> Option<std::time::SystemTime> {
    let id = row.short_id.as_ref()?;
    w.session.read(cx).backend.session_modified(w.host().as_ref(), &row.root, id)
}

/// How long ago `t` was, shortly: "just now", "12m", "3h", "2d".
fn ago(t: std::time::SystemTime) -> String {
    let secs = t.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// Whether a project key is a folder on another machine.
fn is_remote_project(root: &Path) -> bool {
    root.to_string_lossy().starts_with("ssh://")
}

/// A project's name before it is given one: its folder's, with the machine for another one's.
fn project_default_name(root: &Path) -> String {
    let text = root.to_string_lossy();
    match text.strip_prefix("ssh://") {
        Some(rest) => {
            let host = rest.split('/').next().unwrap_or(rest);
            format!("{} ({host})", folder_name(Path::new(rest)))
        }
        None => folder_name(root),
    }
}

/// Make `root` a project, named after its folder (once).
pub fn add_project(root: PathBuf, cx: &mut App) {
    let name = folder_name(&root);
    name_project(root, name, cx);
}

/// Make `root` a project called `name`, or rename it when it is one already.
fn name_project(root: PathBuf, name: String, cx: &mut App) {
    crate::settings_window::change(cx, move |s| match s.projects.iter_mut().find(|p| p.root == root) {
        Some(p) => p.name = name,
        None => s.projects.push(wundur_core::settings::Project { name, root, folded: false }),
    });
}
