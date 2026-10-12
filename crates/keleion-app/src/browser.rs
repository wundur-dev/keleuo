//! The directory browser that opens sessions: shown when the app starts and for "+ New
//! session". Folders on the left (recent ones pinned on top, the current folder's subfolders
//! below, type to filter), each with a live dot when an agent runs there and a count of its
//! recent sessions. The selected folder's sessions on the right: running ones first (Fork here,
//! Take over when it stops), then recent ones (Continue), plus Start a new session and New
//! session in a git worktree.
//!
//! Discovery runs off the UI thread, and only for the folders the list is showing.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleion_core::agent::{Backend, LiveReason, LiveSession, Running, SessionInfo};
use keleion_core::host::Host;

/// What the person chose.
pub enum BrowserEvent {
    /// A new session in `root`, or (`worktree`: its name) in a new git worktree of its repository.
    New { host: Arc<dyn Host>, root: PathBuf, backend: Arc<dyn Backend>, worktree: Option<String> },
    Resume { host: Arc<dyn Host>, root: PathBuf, backend: Arc<dyn Backend>, id: String, fork: bool, wait_for: Option<u32> },
    /// A session already open in this window.
    Switch(String),
    Close,
}

/// A session open in this window: its folder and conversation id.
#[derive(Clone, Debug)]
pub struct OpenSession {
    pub root: PathBuf,
    pub id: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Badge {
    count: usize,
}

#[derive(Clone)]
enum Row {
    Header(&'static str),
    Folder { path: PathBuf, pinned: bool },
    Up,
}

struct Detail {
    folder: PathBuf,
    sessions: Vec<SessionInfo>,
    /// `(pid, session id)` of agents running there outside this window.
    running: Vec<LiveSession>,
    is_git: bool,
    /// The repository the folder is in: its top folder and checked-out branch.
    repo: Option<(PathBuf, Option<String>)>,
    loading: bool,
}

pub struct Browser {
    host: Arc<dyn Host>,
    backends: Vec<Arc<dyn Backend>>,
    backend: Arc<dyn Backend>,
    dir: PathBuf,
    subfolders: Vec<PathBuf>,
    recent: Vec<PathBuf>,
    filter: Entity<InputState>,
    /// The worktree form: its name field, while shown.
    worktree: Option<(Entity<InputState>, Subscription)>,
    rows: Vec<Row>,
    selected: Option<usize>,
    badges: HashMap<PathBuf, Badge>,
    pending: HashSet<PathBuf>,
    running: Vec<Running>,
    open: Vec<OpenSession>,
    detail: Option<Detail>,
    pub note: Option<String>,
    /// Shown over a session (closes on Escape) rather than as the whole window.
    pub overlay: bool,
    scroll: UniformListScrollHandle,
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
        recent: Vec<PathBuf>,
        backends: Vec<Arc<dyn Backend>>,
        open: Vec<OpenSession>,
        overlay: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Type to filter folders"));
        let sub = cx.subscribe_in(&filter, window, |b, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::Change => b.rebuild_rows(cx),
            InputEvent::PressEnter { secondary, .. } => b.open_default(*secondary, window, cx),
            _ => {}
        });
        filter.update(cx, |f, cx| f.focus(window, cx));
        let backend = backends[0].clone();
        let mut this = Self {
            host,
            backends,
            backend,
            dir: dir.clone(),
            subfolders: Vec::new(),
            recent,
            filter,
            worktree: None,
            rows: Vec::new(),
            selected: None,
            badges: HashMap::new(),
            pending: HashSet::new(),
            running: Vec::new(),
            open,
            detail: None,
            note: None,
            overlay,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _tasks: Vec::new(),
            _subs: vec![sub],
        };
        // This folder and the recent ones show at once; the folders here fill in when listed.
        this.rebuild_rows(cx);
        this.go_to(dir.clone(), Some(dir), cx);
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
                    b.rebuild_rows(cx);
                }
            });
        });
        self._tasks.push(task);
    }

    /// List `dir`; select `select` (or the first folder).
    pub fn go_to(&mut self, dir: PathBuf, select: Option<PathBuf>, cx: &mut Context<Self>) {
        self.dir = dir.clone();
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
                b.rebuild_rows(cx);
                let ix = select
                    .and_then(|s| b.rows.iter().position(|r| matches!(r, Row::Folder { path, .. } if *path == s)))
                    .or_else(|| b.rows.iter().position(|r| matches!(r, Row::Folder { .. })));
                if let Some(ix) = ix {
                    b.select(ix, cx);
                }
            });
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        let query = self.filter.read(cx).value().to_lowercase();
        let keep = |p: &Path| query.is_empty() || name_of(p).to_lowercase().contains(&query);
        let selected = self.selected_folder();
        let mut rows = Vec::new();
        // The folder itself first: what `keleion .` means.
        rows.push(Row::Header("This folder"));
        rows.push(Row::Folder { path: self.dir.clone(), pinned: true });
        let recent: Vec<&PathBuf> = self.recent.iter().filter(|p| **p != self.dir && keep(p)).collect();
        if !recent.is_empty() {
            rows.push(Row::Header("Recent"));
            rows.extend(recent.into_iter().map(|p| Row::Folder { path: p.clone(), pinned: true }));
        }
        rows.push(Row::Header("Folders here"));
        if self.dir.parent().is_some() {
            rows.push(Row::Up);
        }
        rows.extend(self.subfolders.iter().filter(|p| keep(p)).map(|p| Row::Folder { path: p.clone(), pinned: false }));
        self.rows = rows;
        self.selected = selected.and_then(|s| self.rows.iter().position(|r| matches!(r, Row::Folder { path, .. } if *path == s)));
        if self.selected.is_none() && !query.is_empty() {
            // Filtering: the first match under "Folders here".
            let first = self.rows.iter().enumerate().find(|(_, r)| matches!(r, Row::Folder { pinned: false, .. })).map(|(i, _)| i);
            if let Some(i) = first {
                self.select(i, cx);
            }
        }
        cx.notify();
    }

    fn selected_folder(&self) -> Option<PathBuf> {
        match self.selected.and_then(|i| self.rows.get(i)) {
            Some(Row::Folder { path, .. }) => Some(path.clone()),
            _ => None,
        }
    }

    /// Look at the selected folder's sessions again (after an open was refused).
    pub fn refresh_detail(&mut self, cx: &mut Context<Self>) {
        if let Some(ix) = self.selected {
            self.detail = None;
            self.select(ix, cx);
        }
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(Row::Folder { path, .. }) = self.rows.get(ix).cloned() else { return };
        self.selected = Some(ix);
        self.worktree = None;
        self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        if self.detail.as_ref().is_some_and(|d| d.folder == path) {
            cx.notify();
            return;
        }
        self.detail = Some(Detail { folder: path.clone(), sessions: Vec::new(), running: Vec::new(), is_git: false, repo: None, loading: true });
        let (host, backend) = (self.host.clone(), self.backend.clone());
        let own: Vec<String> = self.open.iter().filter_map(|o| o.id.clone()).collect();
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let folder = path.clone();
            let (sessions, running, is_git, repo) = cx
                .background_spawn(async move {
                    let caps = backend.capabilities();
                    let sessions = if caps.earlier_sessions { backend.earlier_sessions(host.as_ref(), &folder, 30) } else { Vec::new() };
                    let running = backend.live_sessions(host.as_ref(), &folder, &sessions, std::process::id(), &own);
                    let repo = if host.is_remote() { None } else { crate::shell::repo_of(&folder) };
                    let is_git = repo.is_some();
                    (sessions, running, is_git, repo)
                })
                .await;
            let _ = this.update(cx, |b, cx| {
                if let Some(d) = b.detail.as_mut().filter(|d| d.folder == path) {
                    d.sessions = sessions;
                    d.running = running;
                    d.is_git = is_git;
                    d.repo = repo;
                    d.loading = false;
                    cx.notify();
                }
            });
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn move_selection(&mut self, by: isize, cx: &mut Context<Self>) {
        let n = self.rows.len() as isize;
        let mut i = self.selected.map(|i| i as isize).unwrap_or(-1);
        for _ in 0..n {
            i += by;
            if i < 0 || i >= n {
                return;
            }
            if matches!(self.rows[i as usize], Row::Folder { .. }) {
                self.select(i as usize, cx);
                return;
            }
        }
    }

    fn enter_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.selected_folder() {
            self.filter_clear(window, cx);
            self.go_to(path, None, cx);
        }
    }

    fn go_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(parent) = self.dir.parent().map(Path::to_path_buf) {
            let from = self.dir.clone();
            self.filter_clear(window, cx);
            self.go_to(parent, Some(from), cx);
        }
    }

    fn filter_clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.update(cx, |f, cx| f.set_value("", window, cx));
    }

    /// Enter: a new session in the selected folder (Cmd+Enter: in a git worktree).
    fn open_default(&mut self, worktree: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(root) = self.selected_folder() {
            let is_git = self.detail.as_ref().is_some_and(|d| d.folder == root && d.is_git);
            if worktree && is_git {
                // Never at once: first show what would be made, with a name to change.
                self.ask_worktree(window, cx);
            } else if !worktree {
                cx.emit(BrowserEvent::New { host: self.host.clone(), root, backend: self.backend.clone(), worktree: None });
            }
        }
    }

    /// Show the worktree form for the selected folder's repository, its name suggested.
    fn ask_worktree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(top) = self.detail.as_ref().and_then(|d| d.repo.as_ref()).map(|(t, _)| t.clone()) else { return };
        let suggested = crate::shell::suggest_worktree_name(&top);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(suggested));
        let sub = cx.subscribe_in(&input, window, |b, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { .. } => b.create_worktree(window, cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        });
        input.update(cx, |i, cx| i.focus(window, cx));
        self.worktree = Some((input, sub));
        cx.notify();
    }

    fn create_worktree(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let (Some(root), Some((input, _))) = (self.selected_folder(), self.worktree.as_ref()) else { return };
        let name = input.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        cx.emit(BrowserEvent::New { host: self.host.clone(), root, backend: self.backend.clone(), worktree: Some(name) });
    }

    /// Every agent process on this machine, every few seconds (for the live dots).
    fn watch_running(&mut self, cx: &mut Context<Self>) {
        let (host, backend) = (self.host.clone(), self.backend.clone());
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                let (h, b) = (host.clone(), backend.clone());
                let running = cx.background_spawn(async move { b.running(h.as_ref(), std::process::id()) }).await;
                if this
                    .update(cx, |b, cx| {
                        b.running = running;
                        cx.notify();
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

    /// Count the sessions of the folders in `range` that are not counted yet (the rows on
    /// screen), in one background job.
    fn count_visible(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        let todo: Vec<PathBuf> = self.rows[range.start.min(self.rows.len())..range.end.min(self.rows.len())]
            .iter()
            .filter_map(|r| match r {
                Row::Folder { path, .. } if !self.badges.contains_key(path) && !self.pending.contains(path) => Some(path.clone()),
                _ => None,
            })
            .collect();
        if todo.is_empty() || !self.backend.capabilities().earlier_sessions {
            return;
        }
        self.pending.extend(todo.iter().cloned());
        let (host, backend) = (self.host.clone(), self.backend.clone());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let counted = cx
                .background_spawn(async move {
                    todo.into_iter()
                        .map(|p| {
                            let (count, _) = backend.session_count(host.as_ref(), &p);
                            (p, Badge { count })
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |b, cx| {
                for (p, badge) in counted {
                    b.pending.remove(&p);
                    b.badges.insert(p, badge);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn live_in(&self, folder: &Path) -> bool {
        self.running.iter().any(|r| r.cwd == folder) || self.open.iter().any(|o| o.root == folder)
    }

    fn is_open(&self, id: &str) -> bool {
        self.open.iter().any(|o| o.id.as_deref() == Some(id))
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, hover, sel, green) = (theme.foreground, theme.muted_foreground, theme.secondary_hover, theme.accent, theme.green);
        let base = div().id(("browser-row", ix)).h(px(26.)).px_2().flex().items_center().gap_2().text_sm();
        match &self.rows[ix] {
            Row::Header(title) => base
                .text_size(px(10.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(muted)
                .child(title.to_uppercase())
                .into_any_element(),
            Row::Up => base
                .text_color(muted)
                .cursor_pointer()
                .hover(|d| d.bg(hover))
                .child(Icon::new(IconName::ArrowUp).xsmall())
                .child("Up a folder")
                .on_click(cx.listener(|b, _, window, cx| b.go_up(window, cx)))
                .into_any_element(),
            Row::Folder { path, pinned } => {
                let badge = self.badges.get(path).copied();
                let live = self.live_in(path);
                let selected = self.selected == Some(ix);
                let enter = path.clone();
                // Pinned folders can be anywhere: their name, then where they are.
                let place = pinned.then(|| path.parent().map(short_path).unwrap_or_default());
                base.cursor_pointer()
                    .when(selected, |d| d.bg(sel))
                    .when(!selected, |d| d.hover(|d| d.bg(hover)))
                    .child(Icon::new(IconName::Folder).xsmall().text_color(muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .child(div().flex_none().text_color(fg).child(name_of(path)))
                            .children(place.map(|p| div().min_w_0().truncate().text_xs().text_color(muted).child(p))),
                    )
                    .when(live, |d| {
                        d.child(div().size(px(7.)).rounded_full().bg(green)).tooltip(|w, cx| {
                            gpui_kit::component::tooltip::Tooltip::new("An agent is running here").build(w, cx)
                        })
                    })
                    .when_some(badge.filter(|b| b.count > 0), |d, b| {
                        d.child(
                            div()
                                .px_1()
                                .rounded_sm()
                                .bg(theme.secondary)
                                .text_size(px(10.5))
                                .text_color(muted)
                                .child(format!("{}", b.count)),
                        )
                    })
                    .child(
                        div()
                            .id(("enter", ix))
                            .rounded_sm()
                            .hover(|d| d.bg(theme.border))
                            .child(Icon::new(IconName::ChevronRight).xsmall().text_color(muted))
                            .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Open this folder in the list").build(w, cx))
                            .on_click(cx.listener(move |b, _, window, cx| {
                                cx.stop_propagation();
                                b.filter_clear(window, cx);
                                b.go_to(enter.clone(), None, cx)
                            })),
                    )
                    .on_click(cx.listener(move |b, ev: &ClickEvent, window, cx| {
                        if ev.click_count() >= 2 {
                            b.enter_selected(window, cx);
                        } else {
                            b.select(ix, cx);
                        }
                    }))
                    .into_any_element()
            }
        }
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, hover, amber, border) = (theme.foreground, theme.muted_foreground, theme.secondary_hover, theme.warning, theme.border);
        let Some(d) = &self.detail else {
            return div().p_4().text_sm().text_color(muted).child("Choose a folder").into_any_element();
        };
        let caps = self.backend.capabilities();
        let host = self.host.clone();
        let backend = self.backend.clone();
        let folder = d.folder.clone();
        let mut col = div().flex().flex_col().gap_2().p_3().min_w_0().max_w(px(820.));
        // The repository it is in, named, so a worktree's source is never a surprise.
        let heading = match &d.repo {
            Some((top, branch)) => {
                let inside = folder.strip_prefix(top).ok().filter(|p| !p.as_os_str().is_empty());
                let place = match inside {
                    Some(p) => format!("{} / {}", name_of(top), p.display()),
                    None => name_of(top),
                };
                match branch {
                    Some(b) => format!("{place}  ({b})"),
                    None => place,
                }
            }
            None => name_of(&folder),
        };
        col = col
            .child(div().text_lg().text_color(fg).child(heading))
            .child(div().text_xs().text_color(muted).truncate().child(folder.display().to_string()));
        let (h1, b1, f1) = (host.clone(), backend.clone(), folder.clone());
        let mut actions = div().flex().gap_2().child(
            Button::new("start-new")
                .small()
                .primary()
                .label("Start a new session here")
                .tooltip("Enter")
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(BrowserEvent::New { host: h1.clone(), root: f1.clone(), backend: b1.clone(), worktree: None })
                })),
        );
        if d.is_git && self.worktree.is_none() {
            actions = actions.child(
                Button::new("start-worktree")
                    .small()
                    .label("New session in a git worktree...")
                    .tooltip("Shows the repository, branch and new folder first (Cmd+Enter)")
                    .on_click(cx.listener(|b, _, window, cx| b.ask_worktree(window, cx))),
            );
        }
        col = col.child(actions);
        if let (Some((input, _)), Some((top, branch))) = (self.worktree.as_ref(), d.repo.as_ref()) {
            let name = input.read(cx).value().trim().to_string();
            let repo = name_of(top);
            let from = branch.clone().map(|b| format!(" ({b})")).unwrap_or_default();
            let parent = top.parent().map(|p| p.display().to_string()).unwrap_or_default();
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .text_sm()
                    .child(div().text_color(fg).child(format!("Copy of {repo}{from} into ../{name}, branch keleion/{name}")))
                    .child(div().text_xs().text_color(muted).child(format!("A new folder {parent}/{name} next to {repo}, checked out on a new branch from the current commit.")))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().text_xs().text_color(muted).child("Name"))
                            .child(div().w(px(240.)).child(Input::new(input).small()))
                            .child(Button::new("worktree-create").small().primary().label("Create and start").on_click(cx.listener(
                                |b, _, window, cx| b.create_worktree(window, cx),
                            )))
                            .child(Button::new("worktree-cancel").small().label("Cancel").on_click(cx.listener(|b, _, _, cx| {
                                b.worktree = None;
                                cx.notify();
                            }))),
                    ),
            );
        }
        if self.backends.len() > 1 {
            let mut pick = div().flex().items_center().gap_1().text_xs().text_color(muted).child("Agent:");
            for (i, b) in self.backends.iter().enumerate() {
                let chosen = b.id() == self.backend.id();
                let b = b.clone();
                pick = pick.child(
                    Button::new(("backend", i))
                        .xsmall()
                        .when(chosen, |btn| btn.primary())
                        .when(!chosen, |btn| btn.ghost())
                        .label(b.name())
                        .on_click(cx.listener(move |br, _, _, cx| {
                            br.backend = b.clone();
                            br.badges.clear();
                            br.pending.clear();
                            br.detail = None;
                            if let Some(ix) = br.selected {
                                br.select(ix, cx);
                            }
                        })),
                );
            }
            col = col.child(pick);
        }
        if !caps.earlier_sessions {
            return col.into_any_element();
        }
        if d.loading {
            return col.child(div().text_xs().text_color(muted).child("Looking for sessions...")).into_any_element();
        }
        let section = |title: String| {
            div()
                .mt_2()
                .pb_1()
                .border_b_1()
                .border_color(border)
                .text_size(px(10.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(muted)
                .child(title)
        };
        let since = |pid: u32| self.running.iter().find(|r| r.pid == pid).and_then(|r| r.since.clone());
        if !d.running.is_empty() {
            col = col.child(section("RUNNING NOW".into()));
            for (i, live) in d.running.iter().enumerate() {
                let id = &live.id;
                let info = d.sessions.iter().find(|s| s.id == *id);
                let title = info.map(|s| s.title.clone()).unwrap_or_else(|| id.clone());
                let state = match (live.reason, live.pid) {
                    (LiveReason::RecentWrite, _) | (_, None) => "may be open elsewhere: its file changed seconds ago".to_string(),
                    (_, Some(pid)) => {
                        let when = since(pid).map(|t| format!(" since {t}")).unwrap_or_default();
                        format!("running in another terminal{when} (process {pid})")
                    }
                };
                let (h, b, f, id1, id2, pid) = (host.clone(), backend.clone(), folder.clone(), id.clone(), id.clone(), live.pid);
                let (h2, b2, f2) = (host.clone(), backend.clone(), folder.clone());
                col = col.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .py_1()
                        .child(div().text_sm().text_color(fg).truncate().child(title))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_xs()
                                .text_color(amber)
                                .child(state)
                                .child(crate::panels::short_id(id.clone(), ("running-id", i), cx)),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .when(caps.fork, |d| {
                                    d.child(Button::new(("fork", i)).xsmall().primary().label("Fork here").on_click(cx.listener(
                                        move |_, _, _, cx| {
                                            cx.emit(BrowserEvent::Resume {
                                                host: h.clone(),
                                                root: f.clone(),
                                                backend: b.clone(),
                                                id: id1.clone(),
                                                fork: true,
                                                wait_for: None,
                                            })
                                        },
                                    )))
                                })
                                .when_some(pid, |d, pid| {
                                    d.child(Button::new(("takeover", i)).xsmall().label("Take over when it stops").on_click(cx.listener(
                                        move |_, _, _, cx| {
                                            cx.emit(BrowserEvent::Resume {
                                                host: h2.clone(),
                                                root: f2.clone(),
                                                backend: b2.clone(),
                                                id: id2.clone(),
                                                fork: false,
                                                wait_for: Some(pid),
                                            })
                                        },
                                    )))
                                }),
                        ),
                );
            }
        }
        let recent: Vec<&SessionInfo> = d.sessions.iter().filter(|s| !d.running.iter().any(|l| l.id == s.id)).collect();
        col = col.child(section(if recent.is_empty() { "NO EARLIER SESSIONS HERE".into() } else { "RECENT SESSIONS".into() }));
        for (i, s) in recent.into_iter().enumerate() {
            let open = self.is_open(&s.id);
            let when = s.last_activity.as_deref().map(short_time).unwrap_or_default();
            let (h, b, f, id) = (host.clone(), backend.clone(), folder.clone(), s.id.clone());
            col = col.child(
                div()
                    .id(("recent", i))
                    .px_1()
                    .py_1()
                    .rounded_md()
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .hover(|d| d.bg(hover))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().text_sm().text_color(fg).truncate().child(s.title.clone()))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("{when} · {} messages", s.messages))
                                    .child(crate::panels::short_id(s.id.clone(), ("recent-id", i), cx)),
                            ),
                    )
                    .child(div().text_xs().text_color(muted).child(if open { "Switch to it" } else { "Continue" }))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        if open {
                            cx.emit(BrowserEvent::Switch(id.clone()));
                        } else {
                            cx.emit(BrowserEvent::Resume {
                                host: h.clone(),
                                root: f.clone(),
                                backend: b.clone(),
                                id: id.clone(),
                                fork: false,
                                wait_for: None,
                            });
                        }
                    })),
            );
        }
        col.into_any_element()
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

/// `2026-10-11T22:40:00.617Z` as `10-11 22:40` (UTC).
fn short_time(iso: &str) -> String {
    format!("{} {}", iso.get(5..10).unwrap_or_default(), iso.get(11..16).unwrap_or_default())
}

impl Focusable for Browser {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Browser {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, panel, border, muted, fg) = (theme.background, theme.sidebar, theme.border, theme.muted_foreground, theme.foreground);
        let count = self.rows.len();
        let list = uniform_list(
            "browser-folders",
            count,
            cx.processor(|b: &mut Browser, range: Range<usize>, _, cx| {
                b.count_visible(range.clone(), cx);
                range.map(|i| b.render_row(i, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.scroll)
        .flex_1()
        .min_h_0();
        let overlay = self.overlay;
        let left = div()
            .w(px(340.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(panel)
            .border_r_1()
            .border_color(border)
            .child(
                div()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                Button::new("up")
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::ArrowUp)
                                    .tooltip("Up a folder (Cmd+Up)")
                                    .on_click(cx.listener(|b, _, window, cx| b.go_up(window, cx))),
                            )
                            .child(div().flex_1().min_w_0().truncate().text_xs().text_color(muted).child(short_path(&self.dir))),
                    )
                    .child(Input::new(&self.filter).small()),
            )
            .child(list);
        div()
            .id("browser")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(bg)
            .text_color(fg)
            .capture_key_down(cx.listener(|b, ev: &KeyDownEvent, window, cx| {
                let k = &ev.keystroke;
                let handled = match (k.key.as_str(), k.modifiers.platform) {
                    ("up", false) => {
                        b.move_selection(-1, cx);
                        true
                    }
                    ("down", false) => {
                        b.move_selection(1, cx);
                        true
                    }
                    ("up", true) => {
                        b.go_up(window, cx);
                        true
                    }
                    ("down", true) => {
                        b.enter_selected(window, cx);
                        true
                    }
                    ("enter", true) => {
                        b.open_default(true, window, cx);
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
            .child(
                div()
                    .h(px(30.))
                    .flex_none()
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(border)
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Open a session"))
                    .child(div().text_xs().text_color(muted).child("Enter starts a new one in the selected folder"))
                    .children(self.note.clone().map(|n| div().text_xs().text_color(theme.warning).child(n)))
                    .child(div().flex_1())
                    .when(overlay, |d| {
                        d.child(
                            Button::new("browser-close")
                                .xsmall()
                                .ghost()
                                .icon(IconName::Close)
                                .tooltip("Close (Escape)")
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(BrowserEvent::Close))),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(left)
                    .child(div().id("browser-detail").flex_1().min_w_0().h_full().overflow_y_scroll().child(self.render_detail(cx))),
            )
    }
}
