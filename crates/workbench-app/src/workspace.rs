//! The window: one thin title bar over a dock (gpui-component's DockArea). Everything is a
//! panel that drags into any tab group: the Files, Skills, Terminals and Claude lists,
//! each Claude session, each open file and each terminal. The default layout is the round 2
//! arrangement (lists on the left, Claude, then code); "Reset layout" brings it back, and the
//! arrangement is saved per folder.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::dock::{
    BasePanelView, DockArea, DockAreaState, DockEvent, DockLayout, DockPlacement, DockSkin,
    InsertTarget, PanelBuildContext, PanelId, PanelInfo, PanelStyle, panel_handle, register_panel,
};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Placement, Sizable, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::*;
use workbench_core::host::Host;


use crate::AppSettings;
use crate::actions::*;
use crate::chat_panel::{ChatPanel, ChatPanelEvent};
use crate::agent_session::{AgentSession, SessionEvent, View};
use crate::agent_side::AgentSide;
use crate::doc_panel::{DocContext, DocEvent, DocPanel};
use crate::file_tree::{FileTree, FileTreeEvent};
use crate::panels::{self, TerminalsList, TerminalsListEvent, doc_key, start_tab_rename, term_key};
use crate::skills_panel::{SkillsEvent, SkillsPanel};
use crate::terminal_view::{TerminalEvent, TerminalView};

/// Bumped when the saved layout format changes; older files are ignored.
const LAYOUT_VERSION: usize = 3;

/// What the panel registry needs to rebuild a saved layout for one window.
struct Factory {
    files: Entity<FileTree>,
    skills: Entity<SkillsPanel>,
    claude: Entity<ChatPanel>,
    agent_side: Entity<AgentSide>,
    terms_list: Entity<TerminalsList>,
    doc_ctx: DocContext,
    shell: workbench_core::host::CommandSpec,
    /// Panels made while loading, for the workspace to wire up afterwards.
    made: RefCell<Vec<Made>>,
}

enum Made {
    Doc(Entity<DocPanel>),
    Terminal(Entity<TerminalView>),
}

/// Factories by dock area, so the global panel registry can find the right window.
#[derive(Default)]
struct Factories(HashMap<EntityId, Rc<Factory>>);

impl Global for Factories {}

fn factory(ctx: &PanelBuildContext, cx: &App) -> Option<Rc<Factory>> {
    let area = ctx.dock_area().upgrade()?;
    cx.try_global::<Factories>()?
        .0
        .get(&area.entity_id())
        .cloned()
}

/// Register the panel kinds once, so saved layouts can be rebuilt.
pub fn register_panels(cx: &mut App) {
    cx.set_global(Factories::default());
    for name in ["Files", "Skills", "Agent", "Terminals", "Chat"] {
        register_panel(cx, name, move |ctx, _window, cx| {
            let f = factory(&ctx, cx).expect("a workspace for this dock");
            match name {
                "Files" => panel_handle(f.files.clone()),
                "Skills" => panel_handle(f.skills.clone()),
                "Agent" => panel_handle(f.agent_side.clone()),
                "Chat" => panel_handle(f.claude.clone()),
                _ => panel_handle(f.terms_list.clone()),
            }
        });
    }
    register_panel(cx, "Doc", |ctx, window, cx| {
        let f = factory(&ctx, cx).expect("a workspace for this dock");
        let (path, read_only) = match ctx.info() {
            PanelInfo::Panel(v) => (
                v.get("path")
                    .and_then(|p| p.as_str())
                    .map(PathBuf::from)
                    .unwrap_or_default(),
                v.get("read_only")
                    .and_then(|r| r.as_bool())
                    .unwrap_or(false),
            ),
            _ => (PathBuf::new(), false),
        };
        let doc = cx.new(|cx| DocPanel::new(f.doc_ctx.clone(), path, None, read_only, window, cx));
        f.made.borrow_mut().push(Made::Doc(doc.clone()));
        panel_handle(doc)
    });
    register_panel(cx, "Terminal", |ctx, _window, cx| {
        let f = factory(&ctx, cx).expect("a workspace for this dock");
        // A saved terminal comes back as a fresh shell in the same place.
        let spec = f.shell.clone();
        let view = cx.new(|cx| TerminalView::new(spec, cx));
        f.made.borrow_mut().push(Made::Terminal(view.clone()));
        panel_handle(view)
    });
}

pub enum WorkspaceEvent {
    /// Start a new session (pick a folder).
    NewSession,
    /// This session's Claude wrote or edited a file.
    AgentEdited(PathBuf),
}

impl EventEmitter<WorkspaceEvent> for Workspace {}

pub struct Workspace {
    host: Arc<dyn Host>,
    root: PathBuf,
    branch: Option<String>,
    dock: Entity<DockArea>,
    _skin: Rc<DockSkin>,
    files: Entity<FileTree>,
    skills: Entity<SkillsPanel>,
    /// This session's one Claude conversation.
    pub session: Entity<AgentSession>,
    pub claude: Entity<ChatPanel>,
    agent_side: Entity<AgentSide>,
    /// Shown on the session tab: a git worktree made for this session.
    pub worktree: Option<String>,
    terms_list: Entity<TerminalsList>,
    terminals: Rc<RefCell<Vec<Entity<TerminalView>>>>,
    next_term: usize,
    docs: Vec<Entity<DocPanel>>,
    doc_ctx: DocContext,
    /// Docs and terminals, most recently shown first: where new ones open.
    recent_code: Vec<EntityId>,
    back: Vec<(PathBuf, u32)>,
    forward: Vec<(PathBuf, u32)>,
    save_task: Option<Task<()>>,
    loading: bool,
    first_frame_logged: bool,
    /// When this session was opened (for the ready time).
    made: std::time::Instant,
    focus: FocusHandle,
    panel_subs: HashMap<EntityId, Vec<Subscription>>,
    _watch: Option<workbench_core::host::Watch>,
    _subs: Vec<Subscription>,
}

impl Workspace {
    pub fn new(
        host: Arc<dyn Host>,
        root: PathBuf,
        session: AgentSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let made = std::time::Instant::now();
        let backend = session.backend.clone();
        let files = cx.new(|cx| FileTree::new(host.clone(), root.clone(), cx));
        let skills = cx.new(|cx| SkillsPanel::new(host.clone(), backend.clone(), root.clone(), cx));
        let session = cx.new(|_| session);
        // The view this session was last shown in, else the default for new sessions.
        session.update(cx, |s, cx| s.begin(cx));
        let claude = cx.new(|cx| ChatPanel::new(session.clone(), window, cx));
        let agent_side = cx.new(|cx| AgentSide::new(session.clone(), claude.clone(), cx));
        let terminals = Rc::new(RefCell::new(Vec::new()));
        let terms_list = cx.new(|cx| TerminalsList::new(terminals.clone(), cx));
        let doc_ctx = DocContext {
            host: host.clone(),
            root: root.clone(),
            index: Rc::new(RefCell::new(Arc::new(Default::default()))),
            symbols: Rc::new(RefCell::new(Arc::new(Default::default()))),
        };
        let (dock, skin) = crate::perf::step("workspace dock", || crate::dock_skin::WorkbenchSkin::dock_area("workbench", Some(LAYOUT_VERSION), window, cx));
        skin.set_panel_style(PanelStyle::TabBar, cx);
        skin.set_close_button_visible(true, cx);

        let mut subs = Vec::new();
        subs.push(cx.subscribe_in(
            &skills,
            window,
            |ws, _, ev: &SkillsEvent, window, cx| match ev {
                SkillsEvent::Open(entry) => {
                    let read_only = entry.read_only;
                    ws.open(entry.path.clone(), None, read_only, false, window, cx);
                }
            },
        ));
        subs.push(cx.subscribe_in(
            &files,
            window,
            |ws, _, ev: &FileTreeEvent, window, cx| match ev {
                FileTreeEvent::Open(path) => ws.open(path.clone(), None, false, false, window, cx),
                FileTreeEvent::Indexed(index) => {
                    *ws.doc_ctx.index.borrow_mut() = index.clone();
                    ws.build_symbols(index.clone(), cx);
                    ws.claude.update(cx, |p, cx| {
                        p.index = index.clone();
                        cx.notify();
                    });
                }
            },
        ));
        subs.push(cx.subscribe_in(
            &claude,
            window,
            |ws, _, ev: &ChatPanelEvent, window, cx| match ev {
                ChatPanelEvent::OpenFile(path, line) => {
                    ws.open(path.clone(), *line, false, true, window, cx)
                }
                ChatPanelEvent::NewSession => cx.emit(WorkspaceEvent::NewSession),
                ChatPanelEvent::Closed | ChatPanelEvent::Activated => {}
            },
        ));
        subs.push(cx.subscribe_in(
            &session,
            window,
            |ws, _, ev: &SessionEvent, window, cx| match ev {
                SessionEvent::FileChanged(path) => {
                    ws.file_changed(path, window, cx);
                    if path.ends_with("SKILL.md")
                        || path.components().any(|c| c.as_os_str() == ".claude")
                    {
                        ws.skills.update(cx, |s, cx| s.refresh(None, cx));
                    }
                    cx.emit(WorkspaceEvent::AgentEdited(path.clone()));
                }
                SessionEvent::TurnEnded => ws.files.update(cx, |t, cx| t.refresh(cx)),
            },
        ));
        // The session tab's dot and name follow the conversation.
        subs.push(cx.observe(&session, |_, _, cx| cx.notify()));
        subs.push(cx.subscribe_in(
            &terms_list,
            window,
            |ws, _, ev: &TerminalsListEvent, window, cx| match ev {
                TerminalsListEvent::New => ws.new_terminal(window, cx),
                TerminalsListEvent::Show(t) => {
                    ws.show_panel(PanelId::from(t.entity_id()), window, cx);
                    let handle = t.read(cx).focus.clone();
                    window.focus(&handle, cx);
                }
            },
        ));
        subs.push(
            cx.subscribe_in(&dock, window, |ws, _, ev: &DockEvent, window, cx| {
                if let DockEvent::LayoutChanged = ev {
                    ws.schedule_save(window, cx);
                    cx.notify();
                }
            }),
        );

        let watch = crate::perf::step("workspace watch", || Self::watch_files(&host, &root, window, cx));
        let mut this = Self {
            host,
            root,
            branch: None,
            dock,
            _skin: skin,
            files,
            skills,
            session,
            claude,
            agent_side,
            worktree: None,
            terms_list,
            terminals,
            next_term: 1,
            docs: Vec::new(),
            doc_ctx,
            recent_code: Vec::new(),
            back: Vec::new(),
            forward: Vec::new(),
            save_task: None,
            loading: false,
            first_frame_logged: false,
            made,
            focus: cx.focus_handle(),
            panel_subs: HashMap::new(),
            _watch: watch,
            _subs: subs,
        };
        this.load_branch(cx);
        crate::perf::step("workspace layout", || {
            if !this.restore_layout(window, cx) {
                this.default_layout(window, cx);
            }
        });
        this
    }

    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }

    /// The name in the session list: the first thing asked, else "New session".
    pub fn list_name(&self, cx: &App) -> String {
        self.session.read(cx).chat.title(48).unwrap_or_else(|| "New session".into())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn host(&self) -> Arc<dyn Host> {
        self.host.clone()
    }

    /// Whether a file is open here.
    pub fn has_open(&self, path: &Path, cx: &App) -> bool {
        self.docs.iter().any(|d| d.read(cx).path == path)
    }

    /// Another session in the same folder just edited a file open here: say so on its tab
    /// and as a notification.
    pub fn warn_edited_elsewhere(
        &mut self,
        path: &Path,
        by: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for d in self.docs.clone() {
            if d.read(cx).path == path {
                let by = by.to_string();
                d.update(cx, |d, cx| d.edited_by_other(by, cx));
            }
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        window.push_notification(format!("{name} was just edited by Claude in the session \"{by}\", which shares this folder."), cx);
    }

    pub fn toggle_sidebar_pub(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_sidebar(window, cx);
    }

    pub fn split_right(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Placement::Right, window, cx);
    }

    pub fn focus_main(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.claude.update(cx, |p, cx| p.focus_input(window, cx));
    }

    // ----- layout -----

    fn layout_file(&self) -> Option<PathBuf> {
        let dir = workbench_core::settings::config_dir()?.join("layouts");
        Some(dir.join(format!(
            "{}.json",
            workbench_core::settings::encode_folder(&self.root)
        )))
    }

    /// The round 2 arrangement: the four lists on the left; Claude, then code, side by side.
    fn default_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = true;
        let settings = cx.global::<AppSettings>().0.borrow().layout.clone();
        if self.terminals.borrow().is_empty() && self.docs.is_empty() {
            self.spawn_terminal(window, cx);
        }
        let claude = DockLayout::tabs().panel_view(panel_handle(self.claude.clone()), cx);
        let mut code = DockLayout::tabs();
        for d in &self.docs {
            code = code.panel_view(panel_handle(d.clone()), cx);
        }
        for t in self.terminals.borrow().iter() {
            code = code.panel_view(panel_handle(t.clone()), cx);
        }
        let center = DockLayout::h_split()
            .child(claude, Some(px(settings.claude_width)))
            .child(code, None);
        // Two groups of tool panels, their tabs along the bottom: what you look things up in
        // (files, skills), and what is running (the agent's subagents,
        // terminals). Panels the backend has no use for are left out.
        let caps = self.session.read(cx).backend.capabilities();
        let mut look = DockLayout::tabs().panel_view(panel_handle(self.files.clone()), cx);
        if caps.skills {
            look = look.panel_view(panel_handle(self.skills.clone()), cx);
        }
        let mut running = DockLayout::tabs();
        if caps.subagents {
            running = running.panel_view(panel_handle(self.agent_side.clone()), cx);
        }
        running = running.panel_view(panel_handle(self.terms_list.clone()), cx);
        let left = DockLayout::v_split().child(look, None).child(running, Some(px(300.)));
        self.dock.update(cx, |a, cx| {
            a.set_center(center, window, cx);
            a.set_dock(DockPlacement::Left, left, window, cx);
            a.set_dock_size(DockPlacement::Left, px(settings.sidebar_width), window, cx);
            if settings.sidebar_folded && a.is_dock_open(DockPlacement::Left) {
                a.toggle_dock(DockPlacement::Left, window, cx);
            }
        });
        self.loading = false;
        self.claude.update(cx, |p, cx| p.focus_input(window, cx));
    }

    pub fn reset_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.default_layout(window, cx);
        self.schedule_save(window, cx);
    }

    /// Rebuild the saved arrangement of this folder. False when there is none (or it is from
    /// another version).
    fn restore_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(path) = self.layout_file() else {
            return false;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return false;
        };
        let Ok(state) = serde_json::from_str::<DockAreaState>(&text) else {
            return false;
        };
        if state.version != Some(LAYOUT_VERSION) {
            return false;
        }
        let f = Rc::new(Factory {
            files: self.files.clone(),
            skills: self.skills.clone(),
            claude: self.claude.clone(),
            agent_side: self.agent_side.clone(),
            terms_list: self.terms_list.clone(),
            doc_ctx: self.doc_ctx.clone(),
            shell: self.host.shell_command(&self.root),
            made: RefCell::new(Vec::new()),
        });
        cx.global_mut::<Factories>()
            .0
            .insert(self.dock.entity_id(), f.clone());
        self.loading = true;
        let loaded = self.dock.update(cx, |a, cx| a.load(state, window, cx));
        self.loading = false;
        cx.global_mut::<Factories>()
            .0
            .remove(&self.dock.entity_id());
        if loaded.is_err() {
            return false;
        }
        let made = std::mem::take(&mut *f.made.borrow_mut());
        for m in made {
            match m {
                Made::Doc(doc) => self.adopt_doc(doc, window, cx),
                Made::Terminal(view) => self.adopt_terminal(view, window, cx),
            }
        }
        if !self.in_dock(PanelId::from(self.claude.entity_id()), cx) {
            let view = panel_handle(self.claude.clone());
            self.place(view, None, None, window, cx);
        }
        true
    }

    fn schedule_save(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let Some(path) = self.layout_file() else {
            return;
        };
        let dock = self.dock.downgrade();
        self.save_task = Some(cx.spawn(async move |_, cx: &mut AsyncApp| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(600))
                .await;
            let state = dock.update(cx, |a, cx| a.dump(cx)).ok();
            if let Some(state) = state
                && let Ok(json) = serde_json::to_string_pretty(&state)
            {
                cx.background_spawn(async move {
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    let _ = std::fs::write(&path, json);
                })
                .await;
            }
        }));
    }

    // ----- placing panels -----

    fn find_node(&self, id: PanelId, cx: &App) -> Option<gpui_kit::component::dock::NodeId> {
        let area = self.dock.read(cx);
        [
            DockPlacement::Center,
            DockPlacement::Left,
            DockPlacement::Right,
            DockPlacement::Bottom,
        ]
        .into_iter()
        .find_map(|p| area.layout(p).and_then(|t| t.find_panel_node(id)))
    }

    /// The panels of a tab group.
    fn group_panels(&self, node: gpui_kit::component::dock::NodeId, cx: &App) -> Vec<PanelId> {
        let area = self.dock.read(cx);
        [
            DockPlacement::Center,
            DockPlacement::Left,
            DockPlacement::Right,
            DockPlacement::Bottom,
        ]
        .into_iter()
        .find_map(|p| {
            let tree = area.layout(p)?;
            let mut found = None;
            tree.root().walk(&mut |n| {
                if n.id() == node
                    && let gpui_kit::component::dock::PaneRef::Tabs { panels, .. } = n.kind()
                {
                    found = Some(panels.to_vec());
                }
            });
            found
        })
        .unwrap_or_default()
    }

    fn in_dock(&self, id: PanelId, cx: &App) -> bool {
        self.dock.read(cx).panel(id).is_some()
    }

    /// Add a panel next to `anchor` (same tab group), else beside `beside` (a new group on
    /// that side), else wherever the dock puts it.
    fn place(
        &mut self,
        view: Arc<dyn BasePanelView>,
        anchor: Option<PanelId>,
        beside: Option<(PanelId, Placement)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = view.panel_id(cx);
        let anchor_node = anchor.and_then(|a| self.find_node(a, cx));
        let beside_node = beside.and_then(|(b, p)| self.find_node(b, cx).map(|n| (n, p)));
        self.dock.update(cx, |a, cx| {
            a.add_panel_view(view, DockPlacement::Center, None, window, cx);
            if let Some(node) = anchor_node {
                a.move_panel(
                    id,
                    InsertTarget::Tabs {
                        node,
                        ix: None,
                        activate: true,
                    },
                    window,
                    cx,
                );
            } else if let Some((node, placement)) = beside_node {
                a.move_panel(
                    id,
                    InsertTarget::Split {
                        node,
                        placement,
                        size: None,
                    },
                    window,
                    cx,
                );
            }
            a.select_panel(id, window, cx);
        });
    }

    fn show_panel(&mut self, id: PanelId, window: &mut Window, cx: &mut Context<Self>) {
        let left_closed = self
            .dock
            .read(cx)
            .layout(DockPlacement::Left)
            .is_some_and(|t| t.find_panel_node(id).is_some())
            && !self.dock.read(cx).is_dock_open(DockPlacement::Left);
        self.dock.update(cx, |a, cx| {
            if left_closed {
                a.toggle_dock(DockPlacement::Left, window, cx);
            }
            a.select_panel(id, window, cx);
        });
    }

    fn recent_code_panel(&self, cx: &App) -> Option<PanelId> {
        self.recent_code
            .iter()
            .map(|e| PanelId::from(*e))
            .find(|id| self.in_dock(*id, cx))
    }

    fn any_session_panel(&self, cx: &App) -> Option<PanelId> {
        let id = PanelId::from(self.claude.entity_id());
        self.in_dock(id, cx).then_some(id)
    }

    fn place_code(
        &mut self,
        view: Arc<dyn BasePanelView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let anchor = self.recent_code_panel(cx);
        let beside = self.any_session_panel(cx).map(|s| (s, Placement::Right));
        self.place(view, anchor, beside, window, cx);
    }

    fn touch_code(&mut self, id: EntityId) {
        self.recent_code.retain(|e| *e != id);
        self.recent_code.insert(0, id);
    }

    // ----- files -----

    fn adopt_doc(&mut self, doc: Entity<DocPanel>, window: &mut Window, cx: &mut Context<Self>) {
        let id = doc.entity_id();
        let sub = cx.subscribe_in(
            &doc,
            window,
            move |ws, doc, ev: &DocEvent, window, cx| match ev {
                DocEvent::OpenLink(path, line) => {
                    ws.open(path.clone(), *line, false, true, window, cx)
                }
                DocEvent::Changed => cx.notify(),
                DocEvent::Activated => {
                    ws.touch_code(doc.entity_id());
                    let path = doc.read(cx).path.clone();
                    if path.starts_with(&ws.root) {
                        ws.files.update(cx, |t, cx| t.reveal(&path, cx));
                    }
                }
                DocEvent::Closed => {
                    ws.docs.retain(|d| d != doc);
                    ws.panel_subs.remove(&doc.entity_id());
                }
            },
        );
        self.panel_subs.insert(id, vec![sub]);
        self.touch_code(id);
        self.docs.push(doc);
    }

    /// Open a file (or show it), at a line. `jump` records where we were for Back.
    pub fn open(
        &mut self,
        path: PathBuf,
        line: Option<u32>,
        read_only: bool,
        jump: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if jump && let Some(here) = self.current_location(cx) {
            self.back.push(here);
            self.forward.clear();
        }
        if let Some(doc) = self.docs.iter().find(|d| d.read(cx).path == path).cloned() {
            if let Some(line) = line {
                doc.update(cx, |d, cx| d.go_to_line(line, window, cx));
            }
            self.show_panel(PanelId::from(doc.entity_id()), window, cx);
            doc.update(cx, |d, cx| d.focus_content(window, cx));
            return;
        }
        let ctx = self.doc_ctx.clone();
        let doc = cx.new(|cx| DocPanel::new(ctx, path, line, read_only, window, cx));
        self.adopt_doc(doc.clone(), window, cx);
        self.place_code(panel_handle(doc.clone()), window, cx);
        let handle = doc.read(cx).focus.clone();
        window.focus(&handle, cx);
    }

    fn file_changed(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        for doc in self.docs.clone() {
            if doc.read(cx).path == path {
                doc.update(cx, |d, cx| d.file_changed(window, cx));
            }
        }
    }

    /// The doc with keyboard focus, else the most recently shown one.
    fn current_doc(&self, window: &Window, cx: &App) -> Option<Entity<DocPanel>> {
        self.docs
            .iter()
            .find(|d| d.read(cx).focus.contains_focused(window, cx))
            .cloned()
            .or_else(|| {
                self.recent_code
                    .iter()
                    .find_map(|id| self.docs.iter().find(|d| d.entity_id() == *id).cloned())
            })
    }

    fn current_location(&self, cx: &App) -> Option<(PathBuf, u32)> {
        let doc = self
            .recent_code
            .iter()
            .find_map(|id| self.docs.iter().find(|d| d.entity_id() == *id))?;
        let d = doc.read(cx);
        Some((d.path.clone(), d.cursor_line(cx)))
    }

    fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, line)) = self.back.pop() {
            if let Some(here) = self.current_location(cx) {
                self.forward.push(here);
            }
            self.open(path, Some(line), false, false, window, cx);
        }
    }

    fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, line)) = self.forward.pop() {
            if let Some(here) = self.current_location(cx) {
                self.back.push(here);
            }
            self.open(path, Some(line), false, false, window, cx);
        }
    }

    // ----- terminals -----

    /// A new shell (not yet placed in the dock).
    fn spawn_terminal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalView> {
        let spec = self.host.shell_command(&self.root);
        let view = cx.new(|cx| TerminalView::new(spec, cx));
        self.adopt_terminal(view.clone(), window, cx);
        view
    }

    fn adopt_terminal(
        &mut self,
        view: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let number = self.next_term;
        self.next_term += 1;
        let (root, branch) = (self.root.clone(), self.branch.clone());
        view.update(cx, |v, _| {
            v.number = number;
            v.folder = root;
            v.branch = branch;
        });
        let id = view.entity_id();
        let subs = vec![
            cx.subscribe_in(
                &view,
                window,
                move |ws, view, ev: &TerminalEvent, _, cx| match ev {
                    TerminalEvent::Activated => ws.touch_code(view.entity_id()),
                    TerminalEvent::Closed => {
                        ws.terminals.borrow_mut().retain(|t| t != view);
                        ws.panel_subs.remove(&view.entity_id());
                        ws.terms_list.update(cx, |_, cx| cx.notify());
                    }
                },
            ),
            // The list and the tab title follow the program and unseen output.
            cx.observe(&view, |ws, _, cx| {
                ws.terms_list.update(cx, |_, cx| cx.notify())
            }),
        ];
        self.panel_subs.insert(id, subs);
        self.touch_code(id);
        self.terminals.borrow_mut().push(view);
        self.terms_list.update(cx, |_, cx| cx.notify());
    }

    pub fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let anchor = self.recent_code_panel(cx);
        let beside = self.any_session_panel(cx).map(|s| (s, Placement::Right));
        let view = self.spawn_terminal(window, cx);
        self.place(panel_handle(view.clone()), anchor, beside, window, cx);
        let handle = view.read(cx).focus.clone();
        window.focus(&handle, cx);
    }

    fn current_terminal(&self, window: &Window, cx: &App) -> Option<Entity<TerminalView>> {
        let terms = self.terminals.borrow();
        terms
            .iter()
            .find(|t| t.read(cx).focus.contains_focused(window, cx))
            .cloned()
            .or_else(|| {
                self.recent_code
                    .iter()
                    .find_map(|id| terms.iter().find(|t| t.entity_id() == *id).cloned())
            })
    }

    // ----- background work -----

    /// Outside changes (terminal, other editors): reload open files, re-list the tree.
    fn watch_files(
        host: &Arc<dyn Host>,
        root: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<workbench_core::host::Watch> {
        let (tx, rx) = std::sync::mpsc::channel();
        let watch = host.watch(root, tx)?;
        let (atx, arx) = async_channel::unbounded::<Vec<PathBuf>>();
        std::thread::spawn(move || {
            let noise = |p: &PathBuf| {
                p.components().any(|c| {
                    matches!(
                        c.as_os_str().to_str(),
                        Some("target" | "node_modules" | ".git")
                    )
                })
            };
            while let Ok(change) = rx.recv() {
                let mut paths = change.paths;
                // Debounce: gather what arrives in the next 150 ms.
                while let Ok(more) = rx.recv_timeout(std::time::Duration::from_millis(150)) {
                    paths.extend(more.paths);
                }
                paths.retain(|p| !noise(p));
                paths.sort();
                paths.dedup();
                if paths.is_empty() {
                    continue;
                }
                if atx.send_blocking(paths).is_err() {
                    break;
                }
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(paths) = arx.recv().await {
                let ok = this.update_in(cx, |ws, window, cx| {
                    let mut structure = false;
                    for p in &paths {
                        ws.file_changed(p, window, cx);
                        structure |= !p.exists() || p.is_dir();
                    }
                    if structure || paths.len() > 3 {
                        ws.files.update(cx, |t, cx| t.refresh(cx));
                    }
                });
                if ok.is_err() {
                    break;
                }
            }
        })
        .detach();
        Some(watch)
    }

    fn load_branch(&mut self, cx: &mut Context<Self>) {
        let host = self.host.clone();
        let root = self.root.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let branch = cx
                .background_spawn(async move { host.git_branch(&root) })
                .await;
            let _ = this.update(cx, |ws, cx| {
                ws.branch = branch.clone();
                for t in ws.terminals.borrow().iter() {
                    t.update(cx, |v, cx| {
                        v.branch = branch.clone();
                        cx.notify();
                    });
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Read the project's source files in the background and index their definitions.
    fn build_symbols(
        &mut self,
        index: Arc<workbench_core::links::FileIndex>,
        cx: &mut Context<Self>,
    ) {
        let host = self.host.clone();
        let target = self.doc_ctx.symbols.clone();
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            let started = std::time::Instant::now();
            let symbols = cx
                .background_spawn(async move {
                    let mut out = workbench_core::symbols::SymbolIndex::default();
                    for path in index.files().take(8_000) {
                        if workbench_core::files::language_for(&path) == "text" {
                            continue;
                        }
                        if let Ok(bytes) = host.read(&path)
                            && bytes.len() < 512 * 1024
                            && let Ok(text) = String::from_utf8(bytes)
                        {
                            out.add(workbench_core::symbols::definitions(&path, &text));
                        }
                    }
                    out
                })
                .await;
            if crate::perf::enabled() {
                eprintln!(
                    "perf symbol index: {} definitions in {:?}",
                    symbols.len(),
                    started.elapsed()
                );
            }
            *target.borrow_mut() = Arc::new(symbols);
        })
        .detach();
    }

    // ----- actions -----

    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dock.update(cx, |a, cx| {
            if a.has_dock(DockPlacement::Left) {
                a.toggle_dock(DockPlacement::Left, window, cx);
            }
        });
        let open = self.dock.read(cx).is_dock_open(DockPlacement::Left);
        let settings = cx.global::<AppSettings>().0.clone();
        settings.borrow_mut().layout.sidebar_folded = !open;
        let _ = settings.borrow().save();
        cx.notify();
    }

    /// Zoom the group of the given panel (or zoom out when something is zoomed).
    fn toggle_zoom(&mut self, id: Option<PanelId>, window: &mut Window, cx: &mut Context<Self>) {
        if self.dock.read(cx).is_zoomed() {
            self.dock.update(cx, |a, cx| a.set_zoomed_out(window, cx));
            return;
        }
        if let Some(node) = id.and_then(|id| self.find_node(id, cx)) {
            self.dock
                .update(cx, |a, cx| a.set_zoomed_in(node, window, cx));
        }
    }

    fn focus_claude(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = self.claude.clone();
        self.show_panel(PanelId::from(panel.entity_id()), window, cx);
        panel.update(cx, |p, cx| p.focus_input(window, cx));
    }

    /// The panel with keyboard focus: a doc, a terminal or a session.
    fn focused_panel(&self, window: &Window, cx: &App) -> Option<(PanelId, String, String)> {
        if let Some(d) = self
            .docs
            .iter()
            .find(|d| d.read(cx).focus.contains_focused(window, cx))
        {
            let doc = d.read(cx);
            return Some((PanelId::from(d.entity_id()), doc_key(doc), doc.auto_name()));
        }
        if let Some(t) = self
            .terminals
            .borrow()
            .iter()
            .find(|t| t.read(cx).focus.contains_focused(window, cx))
        {
            let v = t.read(cx);
            return Some((PanelId::from(t.entity_id()), term_key(v), v.auto_name()));
        }
        None
    }

    fn rename_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, key, auto)) = self.focused_panel(window, cx) {
            let current = panels::tab_label(&key, auto.clone(), cx);
            start_tab_rename(key, auto, current, window, cx);
        }
    }

    fn close_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(d) = self
            .docs
            .iter()
            .find(|d| d.read(cx).focus.contains_focused(window, cx))
            .cloned()
        {
            self.dock.update(cx, |a, cx| a.remove_panel(d, window, cx));
        } else if let Some(t) = self
            .current_terminal(window, cx)
            .filter(|t| t.read(cx).focus.contains_focused(window, cx))
        {
            self.dock.update(cx, |a, cx| a.remove_panel(t, window, cx));
        }
    }

    /// Split: move the shown code tab into a new group beside (or below) its group.
    fn split(&mut self, placement: Placement, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.recent_code_panel(cx) else {
            return;
        };
        let Some(node) = self.find_node(id, cx) else {
            return;
        };
        // The tab that stays behind, to show in the old group after the move.
        let stays = self.group_panels(node, cx).into_iter().find(|p| *p != id);
        self.dock.update(cx, |a, cx| {
            // move_panel with a Split target moves the tab; split_at left a copy behind.
            a.move_panel(
                id,
                InsertTarget::Split {
                    node,
                    placement,
                    size: None,
                },
                window,
                cx,
            );
            if let Some(other) = stays {
                a.select_panel(other, window, cx);
            }
            a.select_panel(id, window, cx);
        });
    }

    // ----- control channel (tests and scripting, see control.rs) -----

    pub fn command(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (cmd, arg) = line
            .trim()
            .split_once(' ')
            .map(|(a, b)| (a, b.trim()))
            .unwrap_or((line.trim(), ""));
        match cmd {
            "open" => {
                let (path, line) = match arg.rsplit_once(':') {
                    Some((p, l)) if l.parse::<u32>().is_ok() => (p, l.parse().ok()),
                    _ => (arg, None),
                };
                let path = if Path::new(path).is_absolute() {
                    PathBuf::from(path)
                } else {
                    self.root.join(path)
                };
                self.open(path, line, false, true, window, cx);
            }
            "skill" => {
                let path = self.root.join(".claude/skills").join(arg).join("SKILL.md");
                self.open(path, None, false, false, window, cx);
            }
            "term" => self.new_terminal(window, cx),
            "type" => {
                let text = arg.replace("\\n", "\r");
                if let Some(t) = self
                    .current_terminal(window, cx)
                    .or_else(|| self.terminals.borrow().last().cloned())
                    && let Some(pty) = t.read(cx).pty.clone()
                {
                    pty.write(text.into_bytes());
                }
            }
            "split" => self.split(
                if arg == "down" {
                    Placement::Bottom
                } else {
                    Placement::Right
                },
                window,
                cx,
            ),
            "send" | "allow" | "deny" => {
                let text = arg.replace("\\n", "\n");
                self.session.update(cx, |s, cx| match cmd {
                    "send" => s.send(&text, cx),
                    c => s.answer_first(c == "allow", cx),
                });
            }
            "view" => {
                let view = if arg == "terminal" { View::Terminal } else { View::Chat };
                self.session.update(cx, |s, cx| s.set_view(view, cx));
                return format!("{:?}", self.session.read(cx).view);
            }
            "default-view" => {
                let settings = cx.global::<AppSettings>().0.clone();
                settings.borrow_mut().default_view = arg.to_string();
                let _ = settings.borrow().save();
            }
            "term-screen" => {
                return match self.session.read(cx).terminal_view() {
                    Some(t) => t.read(cx).screen_text().lines().filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" | "),
                    None => "no terminal view".into(),
                };
            }
            "term-type" => {
                // Type into the session's terminal view (tests): `\n` is Enter, `\e` Escape.
                let text = arg.replace("\\n", "\r").replace("\\e", "\u{1b}");
                match self.session.read(cx).terminal_view() {
                    Some(t) => {
                        if let Some(pty) = &t.read(cx).pty {
                            pty.write(text.into_bytes());
                        }
                    }
                    None => return "no terminal view".into(),
                }
            }
            "expand" => {
                let ids: Vec<String> = self
                    .session
                    .read(cx)
                    .chat
                    .agents
                    .iter()
                    .map(|a| a.tool_use_id.clone())
                    .collect();
                self.claude.update(cx, |p, cx| {
                    for id in &ids {
                        p.expand_agent(id, cx);
                    }
                });
            }
            "panel" => {
                // Show a tool panel in its group: files, skills, agent or terminals (tests).
                let id = match arg {
                    "skills" => PanelId::from(self.skills.entity_id()),
                    "agent" => PanelId::from(self.agent_side.entity_id()),
                    "terminals" => PanelId::from(self.terms_list.entity_id()),
                    _ => PanelId::from(self.files.entity_id()),
                };
                self.show_panel(id, window, cx);
            }
            "fold" => self.toggle_sidebar(window, cx),
            "reset" => self.reset_layout(window, cx),
            "zoom" => {
                let id = match arg {
                    "claude" => self.any_session_panel(cx),
                    _ => self.recent_code_panel(cx),
                };
                self.toggle_zoom(id, window, cx);
            }
            "chat" => {
                // The session's chat state, briefly (tests).
                let s = self.session.read(cx);
                let c = &s.chat;
                let edits: Vec<String> = c
                    .items
                    .iter()
                    .filter_map(|i| match i {
                        workbench_core::conversation::Item::Tool(t) => t.diff.as_ref().map(|d| d.path.display().to_string()),
                        _ => None,
                    })
                    .collect();
                return format!(
                    "view={:?} busy={} items={} agents={} tokens={:?} cost={:.4} session={:?} last={:?} edits={:?}",
                    s.view,
                    c.busy,
                    c.items.len(),
                    c.agents.len(),
                    c.last_turn_tokens,
                    c.total_cost_usd,
                    c.session_id,
                    c.last_activity(60),
                    edits
                );
            }
            "state" => {
                let a = self.dock.read(cx);
                let groups = a.layout(DockPlacement::Center).map(|t| {
                    let mut n = 0;
                    t.root().walk(&mut |node| {
                        if matches!(node.kind(), gpui_kit::component::dock::PaneRef::Tabs { .. }) {
                            n += 1;
                        }
                    });
                    n
                });
                return format!(
                    "docs={} terminals={} center_groups={:?} left_open={} zoomed={} focused={}",
                    self.docs.len(),
                    self.terminals.borrow().len(),
                    groups,
                    a.is_dock_open(DockPlacement::Left),
                    a.is_zoomed(),
                    window.focused(cx).is_some()
                );
            }
            _ => return format!("unknown command: {cmd}"),
        }
        "ok".into()
    }

    // ----- rendering -----

    /// The thin strip shown when the sidebar dock is folded: one icon per list.
    fn render_strip(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (bg, border, muted, hover) = (
            theme.sidebar,
            theme.border,
            theme.muted_foreground,
            theme.secondary_hover,
        );
        let items: [(IconName, &'static str, PanelId); 4] = [
            (
                IconName::FolderOpen,
                "Files",
                PanelId::from(self.files.entity_id()),
            ),
            (
                IconName::BookOpen,
                "Skills",
                PanelId::from(self.skills.entity_id()),
            ),
            (
                IconName::SquareTerminal,
                "Terminals",
                PanelId::from(self.terms_list.entity_id()),
            ),
            (
                IconName::Bot,
                "Agent: subagents",
                PanelId::from(self.agent_side.entity_id()),
            ),
        ];
        let mut col = div()
            .w(px(34.))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .pt_1()
            .bg(bg)
            .border_r_1()
            .border_color(border);
        for (i, (icon, tip, id)) in items.into_iter().enumerate() {
            col = col.child(
                div()
                    .id(("strip", i))
                    .size(px(26.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .hover(|d| d.bg(hover))
                    .child(Icon::new(icon).small().text_color(muted))
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tip).build(window, cx)
                    })
                    .on_click(cx.listener(move |ws, _, window, cx| ws.show_panel(id, window, cx))),
            );
        }
        col.into_any_element()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.first_frame_logged {
            self.first_frame_logged = true;
            // A session opened later (from the browser): its own time, with WORKBENCH_PERF=1.
            if crate::perf::enabled() {
                let made = self.made;
                window.on_next_frame(move |_, _| {
                    eprintln!("workbench: workspace ready {:.0} ms after it was opened", made.elapsed().as_secs_f64() * 1000.0);
                });
            }
        }
        let theme = cx.theme();
        let (bg, muted, fg) = (theme.background, theme.muted_foreground, theme.foreground);
        let mut place = self
            .root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if let Some(b) = &self.branch {
            place.push_str(&format!(" ({b})"));
        }
        if self.host.is_remote() {
            place = format!("{}: {place}", self.host.label());
        }
        let left_open = self.dock.read(cx).is_dock_open(DockPlacement::Left);

        let _ = (muted, place);

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(bg)
            .text_color(fg)
            .key_context("Workspace")
            .track_focus(&self.focus)
            .on_action(cx.listener(|ws, _: &ToggleSidebar, w, cx| ws.toggle_sidebar(w, cx)))
            .on_action(cx.listener(|ws, _: &FocusRight, w, cx| {
                let id = ws.recent_code_panel(cx);
                ws.toggle_zoom(id, w, cx)
            }))
            .on_action(cx.listener(|ws, _: &FocusClaude, w, cx| {
                let id = ws.any_session_panel(cx);
                ws.toggle_zoom(id, w, cx)
            }))
            .on_action(cx.listener(|ws, _: &Save, w, cx| {
                if let Some(d) = ws.current_doc(w, cx) {
                    d.update(cx, |d, cx| d.save(w, cx));
                }
            }))
            .on_action(cx.listener(|ws, _: &CloseTab, w, cx| ws.close_focused(w, cx)))
            .on_action(cx.listener(|ws, _: &TogglePreview, w, cx| {
                if let Some(d) = ws.current_doc(w, cx) {
                    d.update(cx, |d, cx| d.toggle_preview(w, cx));
                }
            }))
            .on_action(cx.listener(|ws, _: &RenameTab, w, cx| ws.rename_focused(w, cx)))
            .on_action(cx.listener(|ws, _: &GoBack, w, cx| ws.go_back(w, cx)))
            .on_action(cx.listener(|ws, _: &GoForward, w, cx| ws.go_forward(w, cx)))
            .on_action(cx.listener(|ws, _: &SplitRight, w, cx| ws.split(Placement::Right, w, cx)))
            .on_action(cx.listener(|ws, _: &SplitDown, w, cx| ws.split(Placement::Bottom, w, cx)))
            .on_action(cx.listener(|ws, _: &ResetLayout, w, cx| ws.reset_layout(w, cx)))
            .on_action(cx.listener(|ws, _: &NewTerminal, w, cx| ws.new_terminal(w, cx)))
            .on_action(cx.listener(|ws, _: &AskClaude, w, cx| ws.focus_claude(w, cx)))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .when(!left_open, |d| d.child(self.render_strip(cx)))
                    .child(div().flex_1().min_w_0().h_full().child(self.dock.clone())),
            )
    }
}
