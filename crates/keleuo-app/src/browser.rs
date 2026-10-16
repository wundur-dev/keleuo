//! Create or resume a session: shown when the app starts and for "+ New session". Folders on
//! the left (recent ones, then the folders inside the one being looked at, type to filter or
//! enter a path); on the right the New session form for the selected folder (its name, path and
//! branch; a name, the agent, a new git worktree or not; Create session), and under it the
//! folder's previous sessions, each with Resume and Fork (running ones: Fork here and Take over
//! when it stops). Selecting a folder only changes where a session would start: it never
//! opens one.
//!
//! The folder's sessions, branch and repository are read off the UI thread.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable as _, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleuo_core::agent::{Backend, LiveReason, LiveSession, Running, SessionInfo};
use keleuo_core::host::Host;

/// What the person chose.
pub enum BrowserEvent {
    /// A new session in `root`, or (`worktree`: its name) in a new git worktree of its repository;
    /// `name`: the session's name, when one was given up front.
    New { host: Arc<dyn Host>, root: PathBuf, backend: Arc<dyn Backend>, worktree: Option<String>, name: Option<String> },
    /// An earlier session continued (or, `fork`, copied into a new one named `name`).
    Resume {
        host: Arc<dyn Host>,
        root: PathBuf,
        backend: Arc<dyn Backend>,
        id: String,
        fork: bool,
        wait_for: Option<u32>,
        name: Option<String>,
    },
    /// A session already open in this window.
    Switch(String),
    Close,
}

/// A session open in this window: its conversation id.
#[derive(Clone, Debug)]
pub struct OpenSession {
    pub id: Option<String>,
}

/// The selected folder: its sessions, the agents running there, its repository.
struct Detail {
    folder: PathBuf,
    sessions: Vec<SessionInfo>,
    /// Agents running there outside this window.
    running: Vec<LiveSession>,
    /// The repository the folder is in: its top folder and checked-out branch.
    repo: Option<(PathBuf, Option<String>)>,
    loading: bool,
}

/// A fork being named: the session, the name field.
struct Forking {
    id: String,
    input: Entity<InputState>,
    _sub: Subscription,
}

/// The folder list's width.
const SIDEBAR: f32 = 252.;
/// The form's widest.
const FORM: f32 = 760.;

pub struct Browser {
    host: Arc<dyn Host>,
    backends: Vec<Arc<dyn Backend>>,
    backend: Arc<dyn Backend>,
    /// The folder whose folders show under "Inside".
    dir: PathBuf,
    subfolders: Vec<PathBuf>,
    recent: Vec<PathBuf>,
    /// "Inside" open (its folders listed) or folded.
    inside_open: bool,
    filter: Entity<InputState>,
    /// The new session's name (optional; empty means named after its first message).
    name: Entity<InputState>,
    /// The previous sessions' search.
    search: Entity<InputState>,
    /// Where a new session would start.
    selected: Option<PathBuf>,
    detail: Option<Detail>,
    /// Create in a new git worktree; its name, shown while checked.
    worktree: bool,
    worktree_name: Option<(Entity<InputState>, Subscription)>,
    forking: Option<Forking>,
    running: Vec<Running>,
    open: Vec<OpenSession>,
    /// A path that could not be gone to (beside the filter).
    pub note: Option<String>,
    /// Why the last Create did not (beside the button).
    error: Option<String>,
    /// Create pressed and not refused yet: a second press does nothing.
    creating: bool,
    /// Shown over a session (closes on Escape) rather than as the whole window.
    pub overlay: bool,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<BrowserEvent> for Browser {}

impl Browser {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        host: Arc<dyn Host>,
        dir: PathBuf,
        mut recent: Vec<PathBuf>,
        backends: Vec<Arc<dyn Backend>>,
        open: Vec<OpenSession>,
        overlay: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter folders or enter a path"));
        let sub = cx.subscribe_in(&filter, window, |b, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::Change => {
                b.note = None;
                cx.notify();
            }
            // A path: go there. Filter words: the first match is selected, then the name.
            // Nothing typed: create.
            InputEvent::PressEnter { secondary, .. } => {
                if b.filter.read(cx).value().trim().is_empty() {
                    b.create(*secondary, window, cx);
                } else if !b.go_to_typed(window, cx) {
                    b.select_first_match(window, cx);
                }
            }
            _ => {}
        });
        filter.update(cx, |f, cx| f.focus(window, cx));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Name this session"));
        let name_sub = cx.subscribe_in(&name, window, |b, _, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { secondary, .. } = ev {
                b.create(*secondary, window, cx);
            }
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search sessions"));
        let search_sub = cx.subscribe_in(&search, window, |_, _, ev: &InputEvent, _, cx| {
            if let InputEvent::Change = ev {
                cx.notify();
            }
        });
        // The folder it opened at is the first of the recent ones.
        if !recent.contains(&dir) {
            recent.insert(0, dir.clone());
        }
        let backend = crate::backends::backend(None, cx);
        let backend = backends.iter().find(|b| b.id() == backend.id()).cloned().unwrap_or_else(|| backends[0].clone());
        let mut this = Self {
            host,
            backends,
            backend,
            dir: dir.clone(),
            subfolders: Vec::new(),
            recent,
            inside_open: true,
            filter,
            name,
            search,
            selected: None,
            detail: None,
            worktree: false,
            worktree_name: None,
            forking: None,
            running: Vec::new(),
            open,
            note: None,
            error: None,
            creating: false,
            overlay,
            focus: cx.focus_handle(),
            _tasks: Vec::new(),
            _subs: vec![sub, name_sub, search_sub],
        };
        this.go_to(dir.clone(), cx);
        this.select(dir, cx);
        this.drop_missing_recent(cx);
        this.watch_running(cx);
        this
    }

    /// Recent folders that no longer exist go, checked off the UI thread.
    fn drop_missing_recent(&mut self, cx: &mut Context<Self>) {
        let (host, recent) = (self.host.clone(), self.recent.clone());
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let kept: Vec<PathBuf> = cx.background_spawn(async move { recent.into_iter().filter(|p| host.exists(p)).collect() }).await;
            let _ = this.update(cx, |b, cx| {
                if kept.len() != b.recent.len() {
                    b.recent = kept;
                    cx.notify();
                }
            });
        });
        self._tasks.push(task);
    }

    /// List the folders inside `dir` (shown under "Inside").
    pub fn go_to(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        self.dir = dir.clone();
        self.inside_open = true;
        let host = self.host.clone();
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let listed = cx
                .background_spawn(async move {
                    let mut dirs: Vec<PathBuf> = host
                        .read_dir(&dir)
                        .map(|entries| {
                            entries
                                .into_iter()
                                .filter(|e| e.is_dir && !e.name.starts_with('.'))
                                .map(|e| dir.join(e.name))
                                .collect()
                        })
                        .unwrap_or_default();
                    dirs.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
                    dirs
                })
                .await;
            let _ = this.update(cx, |b, cx| {
                b.subfolders = listed;
                cx.notify();
            });
        });
        self._tasks.push(task);
        cx.notify();
    }

    /// The folders the sidebar shows, filtered, in order: the recent ones, then (when open)
    /// those inside.
    fn visible_folders(&self, cx: &App) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let query = self.filter.read(cx).value().trim().to_lowercase();
        let keep = |p: &&PathBuf| query.is_empty() || name_of(p).to_lowercase().contains(&query);
        let recent = self.recent.iter().filter(keep).cloned().collect();
        let inside = if self.inside_open { self.subfolders.iter().filter(keep).cloned().collect() } else { Vec::new() };
        (recent, inside)
    }

    /// Look at the selected folder's sessions again (after an open was refused).
    pub fn refresh_detail(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.selected.clone() {
            self.detail = None;
            self.select(path, cx);
        }
    }

    /// Make `path` where a new session would start, and read its sessions and repository.
    fn select(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.selected.as_ref() != Some(&path) {
            self.forking = None;
            self.error = None;
            // Its worktree name was suggested for the folder's repository: chosen again.
            self.worktree = false;
            self.worktree_name = None;
        }
        self.selected = Some(path.clone());
        if self.detail.as_ref().is_some_and(|d| d.folder == path) {
            cx.notify();
            return;
        }
        self.detail = Some(Detail { folder: path.clone(), sessions: Vec::new(), running: Vec::new(), repo: None, loading: true });
        let (host, backend) = (self.host.clone(), self.backend.clone());
        let own: Vec<String> = self.open.iter().filter_map(|o| o.id.clone()).collect();
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let folder = path.clone();
            let (sessions, running, repo) = cx
                .background_spawn(async move {
                    let caps = backend.capabilities();
                    let sessions = if caps.earlier_sessions { backend.earlier_sessions(host.as_ref(), &folder, 30) } else { Vec::new() };
                    let running = backend.live_sessions(host.as_ref(), &folder, &sessions, std::process::id(), &own);
                    let repo = if host.is_remote() { None } else { crate::shell::repo_of(&folder) };
                    (sessions, running, repo)
                })
                .await;
            let _ = this.update(cx, |b, cx| {
                if let Some(d) = b.detail.as_mut().filter(|d| d.folder == path) {
                    d.sessions = sessions;
                    d.running = running;
                    d.repo = repo;
                    d.loading = false;
                    // Not a repository: no worktree to make.
                    if d.repo.is_none() {
                        b.worktree = false;
                        b.worktree_name = None;
                    }
                    cx.notify();
                }
            });
        });
        self._tasks.push(task);
        cx.notify();
    }

    /// Up or down the sidebar's folders.
    fn move_selection(&mut self, by: isize, cx: &mut Context<Self>) {
        let (recent, inside) = self.visible_folders(cx);
        let all: Vec<PathBuf> = recent.into_iter().chain(inside).collect();
        if all.is_empty() {
            return;
        }
        let at = self.selected.as_ref().and_then(|s| all.iter().position(|p| p == s));
        let next = match at {
            Some(i) => (i as isize + by).clamp(0, all.len() as isize - 1) as usize,
            None => 0,
        };
        self.select(all[next].clone(), cx);
    }

    /// Filter words and Enter: the first folder that matches is selected, and the name field
    /// is next.
    fn select_first_match(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (recent, inside) = self.visible_folders(cx);
        if let Some(first) = recent.into_iter().chain(inside).next() {
            self.select(first, cx);
            self.name.update(cx, |n, cx| n.focus(window, cx));
        } else {
            self.note = Some("No folder matches.".into());
            cx.notify();
        }
    }

    /// The filter holds a path (`/…` or `~/…`) to a folder: go there and select it. Whether it
    /// was a path.
    fn go_to_typed(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let text = self.filter.read(cx).value().trim().to_string();
        let path = match text.strip_prefix('~') {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => match self.host.home_dir() {
                Some(home) => home.join(rest.trim_start_matches('/')),
                None => return false,
            },
            _ if text.starts_with('/') => PathBuf::from(&text),
            _ => return false,
        };
        if self.host.read_dir(&path).is_err() {
            self.note = Some(format!("No folder at {}.", path.display()));
            cx.notify();
            return true;
        }
        self.show_folder(path, window, cx);
        true
    }

    /// The system's folder picker (this machine only); the chosen folder is shown and selected.
    pub fn choose_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.host.is_remote() {
            return;
        }
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = this.update_in(cx, |b, window, cx| b.show_folder(path, window, cx));
        })
        .detach();
    }

    /// Go to a folder from elsewhere (typed, picked, or dropped from the Finder; a file means
    /// its folder): listed inside, and selected.
    fn show_folder(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let path = if path.is_dir() { path } else { path.parent().map(Path::to_path_buf).unwrap_or(path) };
        self.note = None;
        self.filter_clear(window, cx);
        self.go_to(path.clone(), cx);
        self.select(path, cx);
    }

    fn go_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(parent) = self.dir.parent().map(Path::to_path_buf) {
            self.filter_clear(window, cx);
            self.go_to(parent, cx);
        }
    }

    fn filter_clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.update(cx, |f, cx| f.set_value("", window, cx));
    }

    /// Whether the selected folder is in a git repository (known once its details are read).
    fn is_git(&self) -> bool {
        self.detail.as_ref().is_some_and(|d| Some(&d.folder) == self.selected.as_ref() && d.repo.is_some())
    }

    /// Turn "Create in a new Git worktree" on or off; on, its name is shown to change first.
    fn set_worktree(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.worktree = on && self.is_git();
        self.worktree_name = None;
        if self.worktree
            && let Some(top) = self.detail.as_ref().and_then(|d| d.repo.as_ref()).map(|(t, _)| t.clone())
        {
            let suggested = crate::shell::suggest_worktree_name(&top);
            let input = cx.new(|cx| InputState::new(window, cx).default_value(suggested));
            let sub = cx.subscribe_in(&input, window, |b, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { .. } => b.create(false, window, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            });
            self.worktree_name = Some((input, sub));
        }
        cx.notify();
    }

    /// Tests (and Cmd+Enter): turn the worktree option on, its name shown.
    pub fn ask_worktree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_worktree(true, window, cx);
    }

    /// Create session: a fresh session in the selected folder with the agent, the name and,
    /// when checked, a new worktree. Cmd+Enter turns the worktree on first (its name shows,
    /// to be confirmed with Enter), never creating at once.
    fn create(&mut self, worktree_key: bool, window: &mut Window, cx: &mut Context<Self>) {
        if worktree_key && !self.worktree && self.is_git() {
            self.set_worktree(true, window, cx);
            return;
        }
        if self.creating {
            return;
        }
        let Some(root) = self.selected.clone() else {
            self.error = Some("Choose a folder first.".into());
            cx.notify();
            return;
        };
        let worktree = match (&self.worktree_name, self.worktree) {
            (Some((input, _)), true) => {
                let name = input.read(cx).value().trim().to_string();
                if name.is_empty() {
                    self.error = Some("Give the worktree a name.".into());
                    cx.notify();
                    return;
                }
                Some(name)
            }
            _ => None,
        };
        self.error = None;
        self.creating = true;
        cx.notify();
        let name = self.given_name(cx);
        cx.emit(BrowserEvent::New { host: self.host.clone(), root, backend: self.backend.clone(), worktree, name });
    }

    /// The window could not make the session: say why beside Create, keep what was entered.
    pub fn creation_failed(&mut self, why: String, cx: &mut Context<Self>) {
        self.creating = false;
        self.error = Some(why);
        cx.notify();
    }

    /// The name typed for the new session, if any.
    fn given_name(&self, cx: &App) -> Option<String> {
        Some(self.name.read(cx).value().trim().to_string()).filter(|n| !n.is_empty())
    }

    /// Tests: type a name for the new session and put the cursor in its field.
    pub fn test_name(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.name.update(cx, |n, cx| {
            n.set_value(text.to_string(), window, cx);
            n.focus(window, cx);
        });
    }

    /// Fork: ask the new session's name first (the session's title, marked as a fork).
    fn ask_fork(&mut self, id: String, title: &str, window: &mut Window, cx: &mut Context<Self>) {
        let suggested = fork_name(title);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(suggested));
        let sub = cx.subscribe_in(&input, window, |b, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { .. } => b.fork(window, cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        });
        input.update(cx, |i, cx| {
            i.focus(window, cx);
            i.select_all(window, cx);
        });
        self.forking = Some(Forking { id, input, _sub: sub });
        cx.notify();
    }

    /// The named fork: a new session copied from the one being forked.
    fn fork(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let (Some(root), Some(f)) = (self.selected.clone(), self.forking.as_ref()) else { return };
        let name = Some(f.input.read(cx).value().trim().to_string()).filter(|n| !n.is_empty());
        let id = f.id.clone();
        cx.emit(BrowserEvent::Resume { host: self.host.clone(), root, backend: self.backend.clone(), id, fork: true, wait_for: None, name });
    }

    fn resume(&mut self, id: String, wait_for: Option<u32>, cx: &mut Context<Self>) {
        let Some(root) = self.selected.clone() else { return };
        cx.emit(BrowserEvent::Resume { host: self.host.clone(), root, backend: self.backend.clone(), id, fork: false, wait_for, name: None });
    }

    fn choose_backend(&mut self, backend: Arc<dyn Backend>, cx: &mut Context<Self>) {
        if backend.id() == self.backend.id() {
            return;
        }
        self.backend = backend;
        self.detail = None;
        if let Some(path) = self.selected.clone() {
            self.select(path, cx);
        }
    }

    /// Every agent process on this machine, every few seconds (for the running sessions).
    fn watch_running(&mut self, cx: &mut Context<Self>) {
        let (host, backend) = (self.host.clone(), self.backend.clone());
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                let (h, b) = (host.clone(), backend.clone());
                let running = cx.background_spawn(async move { b.running(h.as_ref(), std::process::id()) }).await;
                if this
                    .update(cx, |b, cx| {
                        if b.running != running {
                            b.running = running;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(std::time::Duration::from_secs(5)).await;
            }
        });
        self._tasks.push(task);
    }

    fn is_open(&self, id: &str) -> bool {
        self.open.iter().any(|o| o.id.as_deref() == Some(id))
    }

    // ----- drawing -----

    fn render_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, hover, border, panel, sel, ring, danger) =
            (theme.foreground, theme.muted_foreground, theme.secondary_hover, theme.border, theme.sidebar, theme.accent, theme.ring, theme.danger);
        let (recent, inside) = self.visible_folders(cx);
        let folder_row = |id: ElementId, path: &PathBuf, cx: &mut Context<Self>| {
            let selected = self.selected.as_ref() == Some(path);
            let (p1, p2) = (path.clone(), path.clone());
            div()
                .id(id)
                .h(px(30.))
                .px_2()
                .rounded_md()
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .when(selected, |d| d.bg(sel))
                .when(!selected, |d| d.hover(|d| d.bg(hover)))
                .child(Icon::new(IconName::Folder).small().text_color(if selected { ring } else { muted }))
                .child(div().flex_1().min_w_0().truncate().text_sm().text_color(fg).child(name_of(path)))
                .when(selected, |d| d.child(Icon::new(IconName::Check).small().text_color(ring)))
                .on_click(cx.listener(move |b, ev: &ClickEvent, window, cx| {
                    // Twice: list what is inside it.
                    if ev.click_count() >= 2 {
                        b.filter_clear(window, cx);
                        b.go_to(p1.clone(), cx);
                    }
                    b.select(p2.clone(), cx);
                }))
        };
        let section = |title: String| div().px_2().pt_3().pb_1().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(muted).child(title);
        let mut list = div().flex().flex_col().gap_px();
        if !recent.is_empty() {
            list = list.child(section("RECENT".into()));
            for (i, p) in recent.iter().enumerate() {
                // A recent folder also shows what is inside it.
                let p2 = p.clone();
                list = list.child(folder_row(ElementId::NamedInteger("recent".into(), i as u64), p, cx).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |b, _, _, cx| {
                        if b.dir != p2 {
                            b.go_to(p2.clone(), cx);
                        }
                    }),
                ));
            }
        }
        let open = self.inside_open;
        let inside_header = div()
            .id("inside")
            .mt_2()
            .h(px(28.))
            .px_2()
            .rounded_md()
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .hover(|d| d.bg(hover))
            .text_sm()
            .text_color(muted)
            .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).small())
            .child(div().min_w_0().truncate().child(format!("Inside {}", name_of(&self.dir))))
            .on_click(cx.listener(|b, _, _, cx| {
                b.inside_open = !b.inside_open;
                cx.notify();
            }));
        let mut inside_list = div().flex().flex_col().gap_px();
        if open {
            if self.dir.parent().is_some() {
                inside_list = inside_list.child(
                    div()
                        .id("parent")
                        .h(px(30.))
                        .px_2()
                        .rounded_md()
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .hover(|d| d.bg(hover))
                        .text_sm()
                        .text_color(muted)
                        .child(Icon::new(IconName::ArrowUp).small())
                        .child("Parent folder")
                        .on_click(cx.listener(|b, _, window, cx| b.go_up(window, cx))),
                );
            }
            for (i, p) in inside.iter().enumerate() {
                inside_list = inside_list.child(folder_row(ElementId::NamedInteger("inside".into(), i as u64), p, cx));
            }
        }
        div()
            .w(px(SIDEBAR))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(panel)
            .border_r_1()
            .border_color(border)
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .pt_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(fg).child("Folders"))
                    .child(Input::new(&self.filter).prefix(Icon::new(IconName::Search).small().text_color(muted)))
                    .children(self.note.clone().map(|n| div().text_xs().text_color(danger).child(n))),
            )
            .child(
                div()
                    .id("folders")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_2()
                    .pb_3()
                    .child(list)
                    .when(!self.host.is_remote(), |d| {
                        d.child(
                            div().px_1().pt_3().pb_3().border_b_1().border_color(border).flex().child(
                                Button::new("choose-folder")
                                    .small()
                                    .outline()
                                    .icon(IconName::FolderOpen)
                                    .label("Choose folder…")
                                    .tooltip("Cmd+O")
                                    .on_click(cx.listener(|b, _, window, cx| b.choose_folder(window, cx))),
                            ),
                        )
                    })
                    .child(inside_header)
                    .child(inside_list),
            )
            .into_any_element()
    }

    fn render_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, inset, danger) = (theme.foreground, theme.muted_foreground, theme.border, theme.secondary, theme.danger);
        let label = |text: &'static str, optional: bool| {
            div()
                .flex()
                .items_baseline()
                .gap_1()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(fg)
                .child(text)
                .when(optional, |d| d.child(div().font_weight(FontWeight::NORMAL).text_color(muted).child("· Optional")))
        };
        // The folder: its name and path, its branch at the right (wrapping under on a narrow
        // window rather than cutting the path).
        let folder = self.selected.clone();
        let branch = self.detail.as_ref().and_then(|d| d.repo.as_ref()).and_then(|(_, b)| b.clone());
        let place = div()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(border)
            .bg(inset)
            .flex()
            .flex_wrap()
            .items_start()
            .justify_between()
            .gap_x_4()
            .gap_y_2()
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_3()
                    .min_w_0()
                    .child(div().pt(px(3.)).child(Icon::new(IconName::Folder).text_color(muted)))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(fg)
                                    .child(folder.as_deref().map(name_of).unwrap_or_else(|| "No folder chosen".into())),
                            )
                            .children(folder.as_deref().map(|p| div().text_sm().text_color(muted).child(p.display().to_string()))),
                    ),
            )
            .children(branch.map(|b| {
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_sm()
                    .text_color(muted)
                    .child(Icon::new(gpui_kit::assets::IconName::GitBranch).small())
                    .child(b)
            }));
        // The agent: the available ones, the default first chosen.
        let me = cx.entity().downgrade();
        let backends = self.backends.clone();
        let chosen = self.backend.id().to_string();
        let agent = Button::new("agent")
            .outline()
            .w_full()
            .label(self.backend.name())
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for b in backends.iter() {
                    let (me, b2) = (me.clone(), b.clone());
                    menu = menu.item(PopupMenuItem::new(b.name()).checked(b.id() == chosen).on_click(move |_, _, cx| {
                        let _ = me.update(cx, |br, cx| br.choose_backend(b2.clone(), cx));
                    }));
                }
                menu
            });
        let fields = div()
            .flex()
            .flex_wrap()
            .gap_4()
            .child(
                div()
                    .flex_1()
                    .min_w(px(240.))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(label("Session name", true))
                    .child(Input::new(&self.name)),
            )
            .child(div().w(px(220.)).flex_none().flex().flex_col().gap_2().child(label("Agent", false)).child(agent));
        // The worktree: for a git folder; else said why not.
        let git = self.is_git();
        let loading = self.detail.as_ref().is_none_or(|d| d.loading);
        let worktree_note = if git || loading {
            "Use a separate working directory for this session.".to_string()
        } else {
            "Not a Git repository, so there is no worktree to make.".to_string()
        };
        let mut worktree = div().flex().flex_col().gap_1().child(
            Checkbox::new("worktree")
                .label("Create in a new Git worktree")
                .checked(self.worktree)
                .disabled(!git)
                .on_click(cx.listener(|b, on: &bool, window, cx| b.set_worktree(*on, window, cx))),
        );
        worktree = worktree.child(div().pl(px(26.)).text_sm().text_color(muted).child(worktree_note));
        if let (Some((input, _)), Some((top, branch))) = (self.worktree_name.as_ref(), self.detail.as_ref().and_then(|d| d.repo.as_ref())) {
            let name = input.read(cx).value().trim().to_string();
            let repo = name_of(top);
            let from = branch.clone().map(|b| format!(" ({b})")).unwrap_or_default();
            let parent = top.parent().map(short_path).unwrap_or_default();
            worktree = worktree.child(
                div()
                    .ml(px(26.))
                    .mt_1()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .child(div().text_sm().text_color(fg).child("Worktree name"))
                            .child(div().w(px(240.)).child(Input::new(input).small())),
                    )
                    .child(div().text_xs().text_color(muted).child(format!(
                        "A copy of {repo}{from} in {parent}/{name}, on a new branch keleuo/{name} from the current commit."
                    ))),
            );
        }
        let actions = div()
            .pt_4()
            .border_t_1()
            .border_color(border)
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_sm().text_color(muted).child("Enter to create"))
                    .children(self.error.clone().map(|e| div().text_sm().text_color(danger).child(e))),
            )
            .child(
                Button::new("create")
                    .primary()
                    .label("Create session")
                    .icon(gpui_kit::assets::IconName::CornerDownLeft)
                    .loading(self.creating)
                    .disabled(self.selected.is_none())
                    .on_click(cx.listener(|b, _, window, cx| b.create(false, window, cx))),
            );
        div()
            .flex()
            .flex_col()
            .gap_6()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_2xl().font_weight(FontWeight::SEMIBOLD).text_color(fg).child("New session"))
                    .child(div().text_sm().text_color(muted).child("Create a new conversation or pick up where you left off.")),
            )
            .child(place)
            .child(fields)
            .child(worktree)
            .child(actions)
            .into_any_element()
    }

    /// The folder's previous sessions: running elsewhere first, then the recent ones.
    fn render_sessions(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        let (fg, muted, border, amber) = (theme.foreground, theme.muted_foreground, theme.border, theme.warning);
        let caps = self.backend.capabilities();
        if !caps.earlier_sessions {
            return None;
        }
        let d = self.detail.as_ref()?;
        let query = self.search.read(cx).value().trim().to_lowercase();
        let keep = |title: &str| query.is_empty() || title.to_lowercase().contains(&query);
        let agent = self.backend.name();
        let since = |pid: u32| self.running.iter().find(|r| r.pid == pid).and_then(|r| r.since.clone());
        let mut rows = div().flex().flex_col();
        let mut shown = 0;
        let row = |id: ElementId| div().id(id).py_3().border_b_1().border_color(border).flex().flex_wrap().items_center().justify_between().gap_3();
        let titles = |title: String, meta: AnyElement| {
            div()
                .flex_1()
                .min_w(px(200.))
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_base().text_color(fg).child(title))
                .child(meta)
        };
        for (i, live) in d.running.iter().enumerate() {
            let info = d.sessions.iter().find(|s| s.id == live.id);
            let title = info.map(|s| s.title.clone()).unwrap_or_else(|| live.id.clone());
            if !keep(&title) {
                continue;
            }
            shown += 1;
            let state = match (live.reason, live.pid) {
                (LiveReason::RecentWrite, _) | (_, None) => "May be open elsewhere: its file changed seconds ago".to_string(),
                (_, Some(pid)) => {
                    let when = since(pid).map(|t| format!(" since {t}")).unwrap_or_default();
                    format!("Running in another terminal{when} (process {pid})")
                }
            };
            let (id1, id2, t1, pid) = (live.id.clone(), live.id.clone(), title.clone(), live.pid);
            let actions = div()
                .flex()
                .gap_2()
                .when(caps.fork, |d| {
                    d.child(Button::new(("fork-live", i)).outline().small().icon(gpui_kit::assets::IconName::GitFork).label("Fork here").on_click(
                        cx.listener(move |b, _, window, cx| b.ask_fork(id1.clone(), &t1, window, cx)),
                    ))
                })
                .when_some(pid, |d, pid| {
                    d.child(Button::new(("takeover", i)).outline().small().label("Take over when it stops").on_click(cx.listener(
                        move |b, _, _, cx| b.resume(id2.clone(), Some(pid), cx),
                    )))
                });
            rows = rows.child(
                row(ElementId::NamedInteger("live".into(), i as u64))
                    .child(titles(title, div().text_sm().text_color(amber).child(state).into_any_element()))
                    .child(actions),
            );
            rows = rows.children(self.render_fork_form(&live.id, cx));
        }
        for (i, s) in d.sessions.iter().filter(|s| !d.running.iter().any(|l| l.id == s.id)).enumerate() {
            if !keep(&s.title) {
                continue;
            }
            shown += 1;
            let open = self.is_open(&s.id);
            let when = s.last_activity.as_deref().map(friendly_time).unwrap_or_default();
            let meta = format!("{agent} · {when} · {} messages", s.messages);
            let (id1, id2, t1) = (s.id.clone(), s.id.clone(), s.title.clone());
            let actions = div()
                .flex()
                .gap_2()
                .when(caps.fork, |d| {
                    d.child(Button::new(("fork", i)).outline().small().icon(gpui_kit::assets::IconName::GitFork).label("Fork").on_click(
                        cx.listener(move |b, _, window, cx| b.ask_fork(id1.clone(), &t1, window, cx)),
                    ))
                })
                .child(if open {
                    Button::new(("switch", i))
                        .outline()
                        .small()
                        .label("Switch to it")
                        .tooltip("Open in this window")
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(BrowserEvent::Switch(id2.clone()))))
                } else {
                    Button::new(("resume", i))
                        .outline()
                        .small()
                        .label("Resume")
                        .icon(IconName::ArrowRight)
                        .on_click(cx.listener(move |b, _, _, cx| b.resume(id2.clone(), None, cx)))
                });
            rows = rows.child(
                row(ElementId::NamedInteger("session".into(), i as u64))
                    .child(titles(s.title.clone(), div().text_sm().text_color(muted).child(meta).into_any_element()))
                    .child(actions),
            );
            rows = rows.children(self.render_fork_form(&s.id, cx));
        }
        let empty = if d.loading {
            Some("Looking for sessions…")
        } else if d.sessions.is_empty() && d.running.is_empty() {
            Some("No previous sessions in this folder.")
        } else if shown == 0 {
            Some("No sessions match.")
        } else {
            None
        };
        Some(
            div()
                .pt_6()
                .border_t_1()
                .border_color(border)
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).text_color(fg).child("Previous sessions"))
                        .child(div().w(px(260.)).child(Input::new(&self.search).prefix(Icon::new(IconName::Search).small().text_color(muted)))),
                )
                .children(empty.map(|e| div().py_3().text_sm().text_color(muted).child(e)))
                .child(rows)
                .into_any_element(),
        )
    }

    /// Under the session being forked: the new session's name, Fork and Cancel.
    fn render_fork_form(&self, id: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        let f = self.forking.as_ref().filter(|f| f.id == id)?;
        let theme = cx.theme();
        let (fg, muted, border) = (theme.foreground, theme.muted_foreground, theme.border);
        Some(
            div()
                .mb_2()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(border)
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_sm().text_color(muted).child("A new session, copied from this one up to now. This one is left as it is."))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(div().text_sm().text_color(fg).child("Name"))
                        .child(div().flex_1().min_w(px(200.)).child(Input::new(&f.input).small()))
                        .child(Button::new("fork-go").small().primary().label("Fork").on_click(cx.listener(|b, _, window, cx| b.fork(window, cx))))
                        .child(Button::new("fork-cancel").small().ghost().label("Cancel").on_click(cx.listener(|b, _, _, cx| {
                            b.forking = None;
                            cx.notify();
                        }))),
                )
                .into_any_element(),
        )
    }
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string())
}

/// A path with the home folder as `~`.
fn short_path(p: &Path) -> String {
    match dirs::home_dir().and_then(|h| p.strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    }
}

/// A fork's suggested name: the session's title, marked, kept short.
fn fork_name(title: &str) -> String {
    let title = title.trim();
    let short: String = title.chars().take(48).collect();
    let short = if short.len() < title.len() { format!("{}…", short.trim_end()) } else { short };
    if short.is_empty() { "Fork".into() } else { format!("{short} (fork)") }
}

/// `2026-10-11T22:40:00.617Z` in local time, as "Today, 16:11", "Yesterday, 20:38" or
/// "Oct 14, 18:40".
fn friendly_time(iso: &str) -> String {
    use chrono::{DateTime, Local};
    let Ok(t) = DateTime::parse_from_rfc3339(iso) else { return String::new() };
    friendly_at(t.with_timezone(&Local).naive_local(), Local::now().naive_local())
}

fn friendly_at(t: chrono::NaiveDateTime, now: chrono::NaiveDateTime) -> String {
    let days = (now.date() - t.date()).num_days();
    let clock = t.format("%H:%M");
    match days {
        0 => format!("Today, {clock}"),
        1 => format!("Yesterday, {clock}"),
        _ => format!("{}, {clock}", t.format("%b %-d")),
    }
}

impl Focusable for Browser {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Browser {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, fg) = (theme.background, theme.foreground);
        let overlay = self.overlay;
        let sidebar = self.render_sidebar(cx);
        let form = self.render_form(cx);
        let sessions = self.render_sessions(cx);
        div()
            .id("browser")
            .track_focus(&self.focus)
            // A folder (or a file in one) dropped from the Finder: go there.
            .on_drop(cx.listener(|b, paths: &ExternalPaths, window, cx| {
                if b.host.is_remote() {
                    return;
                }
                if let Some(p) = paths.paths().first() {
                    b.show_folder(p.clone(), window, cx);
                }
            }))
            .size_full()
            .flex()
            .bg(bg)
            .text_color(fg)
            .capture_key_down(cx.listener(|b, ev: &KeyDownEvent, window, cx| {
                let k = &ev.keystroke;
                let handled = match (k.key.as_str(), k.modifiers.platform) {
                    ("up", false) if b.forking.is_none() => {
                        b.move_selection(-1, cx);
                        true
                    }
                    ("down", false) if b.forking.is_none() => {
                        b.move_selection(1, cx);
                        true
                    }
                    ("up", true) => {
                        b.go_up(window, cx);
                        true
                    }
                    ("o", true) => {
                        b.choose_folder(window, cx);
                        true
                    }
                    ("escape", _) if b.forking.is_some() => {
                        b.forking = None;
                        cx.notify();
                        true
                    }
                    ("escape", _) if b.overlay => {
                        cx.emit(BrowserEvent::Close);
                        true
                    }
                    _ => false,
                };
                if handled {
                    cx.stop_propagation();
                }
            }))
            .child(sidebar)
            .child(
                div()
                    .id("browser-main")
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(FORM + 96.))
                            .px_12()
                            .pt_10()
                            .pb_12()
                            .flex()
                            .flex_col()
                            .gap_8()
                            .child(form)
                            .children(sessions),
                    )
                    .when(overlay, |d| {
                        d.child(
                            div().absolute().top_2().right_2().child(
                                Button::new("browser-close")
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::Close)
                                    .tooltip("Close (Escape)")
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(BrowserEvent::Close))),
                            ),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn times_read_as_today_yesterday_or_a_date() {
        let at = |s: &str| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap();
        let now = at("2026-10-16 18:00");
        assert_eq!(friendly_at(at("2026-10-16 16:11"), now), "Today, 16:11");
        assert_eq!(friendly_at(at("2026-10-15 20:38"), now), "Yesterday, 20:38");
        assert_eq!(friendly_at(at("2026-10-14 18:40"), now), "Oct 14, 18:40");
    }

    #[::core::prelude::v1::test]
    fn a_fork_is_named_after_its_session() {
        assert_eq!(fork_name("Terminal zoom and layout"), "Terminal zoom and layout (fork)");
        assert_eq!(fork_name("  "), "Fork");
        let long = "a".repeat(80);
        assert!(fork_name(&long).ends_with("… (fork)"));
    }
}
