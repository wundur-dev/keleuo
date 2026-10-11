//! The window: a sidebar (Files, Skills, Terminals, Claude sessions; folds to an icon strip),
//! Claude as the main column, and the right area where files, skills and terminals open as
//! tabs (and can split). No bottom panel. Column widths are remembered.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, ResizableState, Sizable, TitleBar, h_resizable, resizable_panel,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use workbench_core::host::Host;
use workbench_core::skills::Source;
use workbench_core::tabs::NameStore;

use crate::AppSettings;
use crate::actions::*;
use crate::claude_panel::{ClaudePanel, ClaudePanelEvent};
use crate::claude_session::{ClaudeSession, SessionEvent};
use crate::file_tree::{FileTree, FileTreeEvent};
use crate::right_area::{RightArea, RightAreaEvent, Split};
use crate::skills_panel::{SkillsEvent, SkillsPanel};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FocusMode {
    /// Only the right area.
    Right,
    /// Only Claude.
    Claude,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Section {
    Files,
    Skills,
    Terminals,
    Sessions,
}

const SECTIONS: [Section; 4] = [
    Section::Files,
    Section::Skills,
    Section::Terminals,
    Section::Sessions,
];

impl Section {
    fn title(self) -> &'static str {
        match self {
            Section::Files => "Files",
            Section::Skills => "Skills",
            Section::Terminals => "Terminals",
            Section::Sessions => "Claude sessions",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Section::Files => IconName::FolderOpen,
            Section::Skills => IconName::BookOpen,
            Section::Terminals => IconName::SquareTerminal,
            Section::Sessions => IconName::Bot,
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

struct SessionTab {
    number: usize,
    session: Entity<ClaudeSession>,
    panel: Entity<ClaudePanel>,
    _subs: Vec<Subscription>,
}

/// An inline rename in the sidebar: the name store key, the automatic name, the field.
struct Rename {
    key: String,
    auto: String,
    input: Entity<InputState>,
    _sub: Subscription,
}

type Select = Rc<dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)>;

/// One sidebar row (a terminal or a session).
struct Row {
    id: SharedString,
    name: String,
    key: String,
    auto: String,
    dot: Option<Hsla>,
    selected: bool,
    icon: IconName,
    select: Select,
}

pub struct Workspace {
    host: Arc<dyn Host>,
    root: PathBuf,
    branch: Option<String>,
    names: Rc<RefCell<NameStore>>,
    skills: Entity<SkillsPanel>,
    tree: Entity<FileTree>,
    pub area: Entity<RightArea>,
    sessions: Vec<SessionTab>,
    active_session: usize,
    next_session: usize,
    sidebar_folded: bool,
    open_sections: [bool; 4],
    focus_mode: Option<FocusMode>,
    renaming: Option<Rename>,
    layout: Entity<ResizableState>,
    save_layout: Option<Task<()>>,
    index: Arc<workbench_core::links::FileIndex>,
    first_frame_logged: bool,
    focus: FocusHandle,
    _watch: Option<workbench_core::host::Watch>,
    _subs: Vec<Subscription>,
}

impl Workspace {
    pub fn new(
        host: Arc<dyn Host>,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let names = Rc::new(RefCell::new(NameStore::open_default()));
        let skills = cx.new(|cx| SkillsPanel::new(host.clone(), root.clone(), cx));
        let tree = cx.new(|cx| FileTree::new(host.clone(), root.clone(), cx));
        let area = cx.new(|cx| RightArea::new(host.clone(), root.clone(), names.clone(), cx));
        let layout = cx.new(|_| ResizableState::default());
        let folded = cx.global::<AppSettings>().0.borrow().layout.sidebar_folded;

        let mut subs = Vec::new();
        subs.push(cx.subscribe_in(
            &skills,
            window,
            |ws, _, ev: &SkillsEvent, window, cx| match ev {
                SkillsEvent::Open(entry) => {
                    let read_only = matches!(entry.source, Source::Plugin(_));
                    let path = entry.path.clone();
                    ws.show_right();
                    ws.area
                        .update(cx, |a, cx| a.open(path, None, read_only, window, cx));
                }
            },
        ));
        subs.push(cx.subscribe_in(
            &tree,
            window,
            |ws, _, ev: &FileTreeEvent, window, cx| match ev {
                FileTreeEvent::Open(path) => {
                    let path = path.clone();
                    ws.show_right();
                    ws.area
                        .update(cx, |a, cx| a.open(path, None, false, window, cx));
                }
                FileTreeEvent::Indexed(index) => {
                    ws.index = index.clone();
                    ws.area.update(cx, |a, _| a.index = index.clone());
                    ws.build_symbols(index.clone(), cx);
                    for s in &ws.sessions {
                        s.panel.update(cx, |p, cx| {
                            p.index = index.clone();
                            cx.notify();
                        });
                    }
                }
            },
        ));
        subs.push(cx.subscribe_in(
            &area,
            window,
            |ws, _, ev: &RightAreaEvent, window, cx| match ev {
                RightAreaEvent::Activated(path) => {
                    let path = path.clone();
                    if path.starts_with(&ws.root) {
                        ws.tree.update(cx, |t, cx| t.reveal(&path, cx));
                    }
                }
                RightAreaEvent::OpenLink(path, line) => {
                    ws.open_file(path.clone(), *line, window, cx)
                }
            },
        ));
        // The sidebar lists terminals: redraw when the area changes.
        subs.push(cx.observe(&area, |_, _, cx| cx.notify()));

        let watch = Self::watch_files(&host, &root, window, cx);

        let mut this = Self {
            host,
            root,
            branch: None,
            names,
            skills,
            tree,
            area,
            sessions: Vec::new(),
            active_session: 0,
            next_session: 1,
            sidebar_folded: folded,
            open_sections: [true, true, true, true],
            focus_mode: None,
            renaming: None,
            layout,
            save_layout: None,
            index: Arc::new(Default::default()),
            first_frame_logged: false,
            focus: cx.focus_handle(),
            _watch: watch,
            _subs: subs,
        };
        this.load_branch(cx);
        this.new_session(window, cx);
        this
    }

    /// Outside changes (terminal, other editors): reload open files, re-list the tree.
    fn watch_files(
        host: &Arc<dyn Host>,
        root: &std::path::Path,
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
                        ws.area.update(cx, |a, cx| a.file_changed(p, window, cx));
                        structure |= !p.exists() || p.is_dir();
                    }
                    if structure || paths.len() > 3 {
                        ws.tree.update(cx, |t, cx| t.refresh(cx));
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
                ws.branch = branch;
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
        let target = self.area.read(cx).symbols.clone();
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

    // ----- sessions -----

    pub fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = cx.new(|_| ClaudeSession::new(self.host.clone(), self.root.clone()));
        let panel = cx.new(|cx| ClaudePanel::new(session.clone(), window, cx));
        let index = self.index.clone();
        panel.update(cx, |p, _| p.index = index);
        let subs = vec![
            cx.subscribe_in(
                &panel,
                window,
                |ws, _, ev: &ClaudePanelEvent, window, cx| match ev {
                    ClaudePanelEvent::OpenFile(path, line) => {
                        ws.open_file(path.clone(), *line, window, cx)
                    }
                    ClaudePanelEvent::NewSession => ws.new_session(window, cx),
                },
            ),
            cx.subscribe_in(
                &session,
                window,
                |ws, _, ev: &SessionEvent, window, cx| match ev {
                    SessionEvent::FileChanged(path) => {
                        let path = path.clone();
                        ws.area
                            .update(cx, |a, cx| a.file_changed(&path, window, cx));
                        if path.ends_with("SKILL.md")
                            || path.components().any(|c| c.as_os_str() == ".claude")
                        {
                            ws.skills.update(cx, |s, cx| s.refresh(None, cx));
                        }
                    }
                    SessionEvent::TurnEnded => ws.tree.update(cx, |t, cx| t.refresh(cx)),
                },
            ),
            // Sidebar dots follow the session state.
            cx.observe(&session, |_, _, cx| cx.notify()),
        ];
        let number = self.next_session;
        self.next_session += 1;
        self.sessions.push(SessionTab {
            number,
            session,
            panel,
            _subs: subs,
        });
        self.switch_session(self.sessions.len() - 1, window, cx);
    }

    fn switch_session(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.sessions.len() {
            return;
        }
        self.active_session = ix;
        if self.focus_mode == Some(FocusMode::Right) {
            self.focus_mode = None;
        }
        let s = &self.sessions[ix];
        s.session.update(cx, |s, _| s.unseen = false);
        s.panel.update(cx, |p, cx| p.focus_input(window, cx));
        cx.notify();
    }

    fn close_session(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.sessions.len() {
            return;
        }
        let tab = self.sessions.remove(ix);
        tab.session.update(cx, |s, _| s.stop());
        if self.sessions.is_empty() {
            self.new_session(window, cx);
            return;
        }
        self.active_session = self.active_session.min(self.sessions.len() - 1);
        self.switch_session(self.active_session, window, cx);
    }

    fn session_key(&self, number: usize) -> String {
        NameStore::key(&self.root, &format!("session:{number}"))
    }

    fn session_auto_name(&self, tab: &SessionTab, cx: &App) -> String {
        tab.session
            .read(cx)
            .chat
            .title(40)
            .unwrap_or_else(|| format!("Session {}", tab.number))
    }

    fn session_name(&self, tab: &SessionTab, cx: &App) -> String {
        let key = self.session_key(tab.number);
        self.names
            .borrow()
            .get(&key)
            .map(str::to_string)
            .unwrap_or_else(|| self.session_auto_name(tab, cx))
    }

    fn active_session(&self) -> Option<&SessionTab> {
        self.sessions.get(self.active_session)
    }

    // ----- right area -----

    fn show_right(&mut self) {
        if self.focus_mode == Some(FocusMode::Claude) {
            self.focus_mode = None;
        }
    }

    fn open_file(
        &mut self,
        path: PathBuf,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_right();
        self.area.update(cx, |a, cx| a.jump(path, line, window, cx));
        cx.notify();
    }

    fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_right();
        self.open_sections[Section::Terminals.index()] = true;
        self.area.update(cx, |a, cx| {
            a.new_terminal(window, cx);
        });
        cx.notify();
    }

    // ----- layout -----

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_folded = !self.sidebar_folded;
        let settings = cx.global::<AppSettings>().0.clone();
        settings.borrow_mut().layout.sidebar_folded = self.sidebar_folded;
        let _ = settings.borrow().save();
        cx.notify();
    }

    fn set_focus_mode(&mut self, mode: FocusMode, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_mode = if self.focus_mode == Some(mode) {
            None
        } else {
            Some(mode)
        };
        match self.focus_mode {
            Some(FocusMode::Claude) => {
                if let Some(s) = self.active_session() {
                    s.panel.update(cx, |p, cx| p.focus_input(window, cx));
                }
            }
            Some(FocusMode::Right) => self.area.update(cx, |a, cx| a.focus_active(window, cx)),
            None => {}
        }
        cx.notify();
    }

    /// Remember column widths a moment after the user stops dragging.
    fn layout_changed(&mut self, cx: &mut Context<Self>) {
        let sizes = self.layout.read(cx).sizes().clone();
        let folded = self.sidebar_folded;
        self.save_layout = Some(cx.spawn(async move |_, cx: &mut AsyncApp| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(400))
                .await;
            cx.update(|cx| {
                let settings = cx.global::<AppSettings>().0.clone();
                let mut s = settings.borrow_mut();
                if !folded
                    && let Some(w) = sizes.first().map(|p| f32::from(*p)).filter(|w| *w > 60.0)
                {
                    s.layout.sidebar_width = w;
                }
                if let Some(w) = sizes.get(1).map(|p| f32::from(*p)).filter(|w| *w > 60.0) {
                    s.layout.claude_width = w;
                }
                let _ = s.save();
            });
        }));
    }

    // ----- sidebar renames -----

    fn start_rename(
        &mut self,
        key: String,
        auto: String,
        current: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        input.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        let sub = cx.subscribe_in(&input, window, |ws, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::PressEnter { .. } | InputEvent::Blur => ws.commit_rename(cx),
            _ => {}
        });
        self.renaming = Some(Rename {
            key,
            auto,
            input,
            _sub: sub,
        });
        cx.notify();
    }

    fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.renaming.take() else {
            return;
        };
        let text = r.input.read(cx).value().trim().to_string();
        let value = if text == r.auto { "" } else { text.as_str() };
        let _ = self.names.borrow_mut().set(&r.key, value);
        self.area.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    fn reset_name(&mut self, key: &str, cx: &mut Context<Self>) {
        let _ = self.names.borrow_mut().reset(key);
        self.area.update(cx, |_, cx| cx.notify());
        cx.notify();
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
                let path = if PathBuf::from(path).is_absolute() {
                    PathBuf::from(path)
                } else {
                    self.root.join(path)
                };
                self.open_file(path, line, window, cx);
            }
            "skill" => {
                // Open a project skill by folder name.
                let path = self.root.join(".claude/skills").join(arg).join("SKILL.md");
                self.show_right();
                self.area
                    .update(cx, |a, cx| a.open(path, None, false, window, cx));
            }
            "term" => self.new_terminal(window, cx),
            "type" => {
                // Text for the shown terminal; `\n` becomes Enter.
                let text = arg.replace("\\n", "\r");
                self.area.read(cx).write_terminal(&text, cx);
            }
            "split" => {
                let how = if arg == "down" {
                    Split::Down
                } else {
                    Split::Right
                };
                self.area.update(cx, |a, cx| a.split(how, cx));
            }
            "unsplit" => self.area.update(cx, |a, cx| a.unsplit(cx)),
            "move" => self.area.update(cx, |a, cx| {
                if let Some(id) = a.active_id() {
                    let to = 1 - a.focused_pane;
                    a.move_to_pane(id, to, cx);
                }
            }),
            "session" => {
                if arg == "new" {
                    self.new_session(window, cx);
                } else if let Ok(n) = arg.parse::<usize>() {
                    self.switch_session(n.saturating_sub(1), window, cx);
                }
            }
            "send" => {
                if let Some(s) = self.active_session() {
                    let text = arg.to_string();
                    s.session.update(cx, |s, cx| s.send(&text, cx));
                }
            }
            "allow" | "deny" => {
                if let Some(s) = self.active_session() {
                    let allow = cmd == "allow";
                    s.session.update(cx, |s, cx| s.answer_first(allow, cx));
                }
            }
            "fold" => self.toggle_sidebar(cx),
            "focus" => match arg {
                "claude" => self.set_focus_mode(FocusMode::Claude, window, cx),
                "right" => self.set_focus_mode(FocusMode::Right, window, cx),
                _ => {
                    self.focus_mode = None;
                    cx.notify();
                }
            },
            "section" => {
                if let Some(s) = SECTIONS
                    .iter()
                    .find(|s| s.title().to_lowercase().starts_with(&arg.to_lowercase()))
                {
                    let i = s.index();
                    self.open_sections[i] = !self.open_sections[i];
                    cx.notify();
                }
            }
            "rename-session" => {
                if let Some(tab) = self.active_session() {
                    let key = self.session_key(tab.number);
                    let _ = self.names.borrow_mut().set(&key, arg);
                    cx.notify();
                }
            }
            "state" => {
                let a = self.area.read(cx);
                return format!(
                    "sessions={} active={} folded={} split={:?} focus={:?} terminals={}",
                    self.sessions.len(),
                    self.active_session + 1,
                    self.sidebar_folded,
                    a.split,
                    self.focus_mode,
                    a.terminals(cx).len()
                );
            }
            _ => return format!("unknown command: {cmd}"),
        }
        "ok".into()
    }

    // ----- rendering -----

    fn render_row(&self, row: Row, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, sel_bg, hover) = (
            theme.foreground,
            theme.muted_foreground,
            theme.accent,
            theme.secondary_hover,
        );
        let editing = self
            .renaming
            .as_ref()
            .filter(|r| r.key == row.key)
            .map(|r| r.input.clone());
        let base = div()
            .id(row.id)
            .h(px(22.))
            .pl(px(18.))
            .pr_1()
            .flex()
            .items_center()
            .gap_1()
            .text_xs()
            .text_color(fg)
            .when(row.selected, |d| d.bg(sel_bg))
            .hover(|d| d.bg(hover))
            .child(Icon::new(row.icon).xsmall().text_color(muted));
        if let Some(input) = editing {
            return base
                .child(
                    div()
                        .flex_1()
                        .on_key_down(cx.listener(|ws, ev: &KeyDownEvent, _, cx| {
                            if ev.keystroke.key == "escape" {
                                ws.renaming = None;
                                cx.notify();
                                cx.stop_propagation();
                            }
                        }))
                        .child(Input::new(&input).xsmall()),
                )
                .into_any_element();
        }
        let renamed = self.names.borrow().get(&row.key).is_some();
        let weak = cx.entity().downgrade();
        let (key, auto, name, select) = (row.key, row.auto, row.name, row.select);
        let (k1, a1, n1) = (key.clone(), auto.clone(), name.clone());
        base.child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .when(renamed, |d| d.italic())
                .child(name.clone()),
        )
        .children(
            row.dot
                .map(|c| div().size(px(7.)).flex_none().rounded_full().bg(c)),
        )
        .on_click(cx.listener(move |ws, ev: &ClickEvent, window, cx| {
            if ev.click_count() >= 2 {
                ws.start_rename(k1.clone(), a1.clone(), n1.clone(), window, cx);
            } else {
                select(ws, window, cx);
            }
        }))
        .context_menu(move |menu, _, _| {
            let (w1, w2) = (weak.clone(), weak.clone());
            let (k2, a2, n2, k3) = (key.clone(), auto.clone(), name.clone(), key.clone());
            menu.item(PopupMenuItem::new("Rename").on_click(move |_, window, cx| {
                let _ = w1.update(cx, |ws, cx| {
                    ws.start_rename(k2.clone(), a2.clone(), n2.clone(), window, cx)
                });
            }))
            .item(PopupMenuItem::new("Reset name").on_click(move |_, _, cx| {
                let _ = w2.update(cx, |ws, cx| ws.reset_name(&k3, cx));
            }))
        })
        .into_any_element()
    }

    fn section_header(
        &self,
        section: Section,
        count: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (muted, hover) = (theme.muted_foreground, theme.secondary_hover);
        let open = self.open_sections[section.index()];
        let ix = section.index();
        let action: Option<(&'static str, &'static str)> = match section {
            Section::Files => None,
            Section::Skills => Some(("add-skill", "New project skill")),
            Section::Terminals => Some(("add-term", "New terminal (Cmd+T)")),
            Section::Sessions => Some(("add-session", "New session (Cmd+N)")),
        };
        div()
            .id(("section", ix))
            .h(px(22.))
            .flex_none()
            .px_1()
            .flex()
            .items_center()
            .gap_1()
            .text_size(px(11.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(muted)
            .hover(|d| d.bg(hover))
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall(),
            )
            .child(div().flex_1().child(section.title().to_uppercase()))
            .children(count.map(|c| div().child(c.to_string())))
            .on_click(cx.listener(move |ws, _, _, cx| {
                ws.open_sections[ix] = !ws.open_sections[ix];
                cx.notify();
            }))
            .children(action.map(|(id, tip)| {
                div()
                    .id(id)
                    .rounded_sm()
                    .hover(|d| d.bg(hover))
                    .child(Icon::new(IconName::Plus).xsmall())
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tip).build(window, cx)
                    })
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        cx.stop_propagation();
                        match section {
                            Section::Skills => ws.skills.update(cx, |s, cx| s.new_skill(cx)),
                            Section::Terminals => ws.new_terminal(window, cx),
                            Section::Sessions => ws.new_session(window, cx),
                            Section::Files => {}
                        }
                    }))
            }))
            .into_any_element()
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (bg, border, blue, amber, fg, muted) = (
            theme.sidebar,
            theme.border,
            theme.blue,
            theme.warning,
            theme.foreground,
            theme.muted_foreground,
        );
        let mut col = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(bg)
            .border_r_1()
            .border_color(border);
        // Files and Skills share the free height; Terminals and Sessions take what they need.
        for section in SECTIONS {
            let open = self.open_sections[section.index()];
            match section {
                Section::Files | Section::Skills => {
                    col = col.child(self.section_header(section, None, cx));
                    if open {
                        let body = if section == Section::Files {
                            self.tree.clone().into_any_element()
                        } else {
                            self.skills.clone().into_any_element()
                        };
                        col = col.child(div().flex_1().min_h(px(80.)).child(body));
                    }
                }
                Section::Terminals => {
                    let terms = self.area.read(cx).terminals(cx);
                    col = col.child(self.section_header(section, Some(terms.len()), cx));
                    if open {
                        let mut list = div().flex_none().flex().flex_col();
                        if terms.is_empty() {
                            list = list.child(
                                div()
                                    .pl(px(18.))
                                    .h(px(22.))
                                    .text_xs()
                                    .text_color(muted)
                                    .child("None yet (Cmd+T)"),
                            );
                        }
                        for t in terms {
                            let id = t.id;
                            let row = Row {
                                id: SharedString::from(format!("term-row-{id}")),
                                auto: t.name.clone(),
                                name: t.name,
                                key: t.key,
                                dot: t.unseen.then_some(fg),
                                selected: t.shown,
                                icon: IconName::SquareTerminal,
                                select: Rc::new(move |ws, window, cx| {
                                    ws.show_right();
                                    ws.area.update(cx, |a, cx| a.show(id, window, cx));
                                }),
                            };
                            list = list.child(self.render_row(row, cx));
                        }
                        col = col.child(list);
                    }
                }
                Section::Sessions => {
                    col = col.child(self.section_header(section, Some(self.sessions.len()), cx));
                    if open {
                        let mut list = div().flex_none().flex().flex_col();
                        for (ix, tab) in self.sessions.iter().enumerate() {
                            let s = tab.session.read(cx);
                            let dot = if s.chat.needs_input() {
                                Some(amber)
                            } else if s.chat.busy {
                                Some(blue)
                            } else if s.unseen && ix != self.active_session {
                                Some(fg)
                            } else {
                                None
                            };
                            let row = Row {
                                id: SharedString::from(format!("session-row-{}", tab.number)),
                                name: self.session_name(tab, cx),
                                key: self.session_key(tab.number),
                                auto: self.session_auto_name(tab, cx),
                                dot,
                                selected: ix == self.active_session,
                                icon: IconName::Bot,
                                select: Rc::new(move |ws, window, cx| {
                                    ws.switch_session(ix, window, cx)
                                }),
                            };
                            list = list.child(self.render_row(row, cx));
                        }
                        col = col.child(list);
                    }
                }
            }
        }
        col.into_any_element()
    }

    fn render_strip(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (bg, border, muted, hover) = (
            theme.sidebar,
            theme.border,
            theme.muted_foreground,
            theme.secondary_hover,
        );
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
        for section in SECTIONS {
            let ix = section.index();
            col = col.child(
                div()
                    .id(("strip", ix))
                    .size(px(26.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .hover(|d| d.bg(hover))
                    .child(Icon::new(section.icon()).small().text_color(muted))
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(section.title())
                            .build(window, cx)
                    })
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        ws.open_sections[ix] = true;
                        ws.toggle_sidebar(cx);
                    })),
            );
        }
        col.into_any_element()
    }

    fn title_button(
        &self,
        id: &'static str,
        icon: IconName,
        tip: &'static str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let hover = cx.theme().secondary_hover;
        let muted = cx.theme().muted_foreground;
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
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(tip).build(window, cx)
            })
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.first_frame_logged {
            self.first_frame_logged = true;
            window.on_next_frame(|_, _| {
                if let Some(start) = crate::START.get() {
                    eprintln!(
                        "workbench: first frame {:.0} ms after start",
                        start.elapsed().as_secs_f64() * 1000.0
                    );
                }
            });
        }
        let theme = cx.theme();
        let (bg, muted, warning, fg) = (
            theme.background,
            theme.muted_foreground,
            theme.warning,
            theme.foreground,
        );
        let folder = self
            .root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut place = folder;
        if let Some(b) = &self.branch {
            place.push_str(&format!(" ({b})"));
        }
        if self.host.is_remote() {
            place = format!("{}: {place}", self.host.label());
        }
        let session_name = self
            .active_session()
            .map(|t| self.session_name(t, cx))
            .unwrap_or_default();
        let focus_label = match self.focus_mode {
            Some(FocusMode::Right) => Some("focus: files"),
            Some(FocusMode::Claude) => Some("focus: Claude"),
            None => None,
        };
        let split = self.area.read(cx).split;

        let top = TitleBar::new().h(px(30.)).pl(px(76.)).child(
            div()
                .size_full()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    self.title_button("t-sidebar", IconName::PanelLeft, "Sidebar (Cmd+B)", cx)
                        .on_click(cx.listener(|ws, _, _, cx| ws.toggle_sidebar(cx))),
                )
                .child(
                    div()
                        .flex_none()
                        .px_1()
                        .text_size(px(11.))
                        .text_color(fg)
                        .child(session_name),
                )
                .child(div().flex_1())
                .when_some(focus_label, |d, l| {
                    d.child(
                        div()
                            .flex_none()
                            .px_1()
                            .text_size(px(11.))
                            .text_color(warning)
                            .child(l),
                    )
                })
                .child(
                    div()
                        .flex_none()
                        .px_1()
                        .text_size(px(11.))
                        .text_color(muted)
                        .child(place),
                )
                .child(
                    self.title_button(
                        "t-split",
                        if split == Some(Split::Down) {
                            IconName::PanelBottom
                        } else {
                            IconName::PanelRight
                        },
                        "Split the right area (Cmd+\\; Cmd+Shift+\\ splits below)",
                        cx,
                    )
                    .on_click(cx.listener(|ws, _, _, cx| {
                        ws.area.update(cx, |a, cx| a.toggle_split(Split::Right, cx))
                    })),
                )
                .child(div().w(px(4.))),
        );

        let (side_vis, claude_vis, right_vis) = match self.focus_mode {
            Some(FocusMode::Right) => (false, false, true),
            Some(FocusMode::Claude) => (false, true, false),
            None => (!self.sidebar_folded, true, true),
        };
        let layout = cx.global::<AppSettings>().0.borrow().layout.clone();
        let claude = self.active_session().map(|t| t.panel.clone());
        let weak = cx.entity().downgrade();
        let columns = h_resizable("main")
            .with_state(&self.layout)
            .on_resize(move |_, _, cx| {
                let _ = weak.update(cx, |ws, cx| ws.layout_changed(cx));
            })
            .child(
                resizable_panel()
                    .size(px(layout.sidebar_width))
                    .size_range(px(150.)..px(480.))
                    .visible(side_vis)
                    .child(self.render_sidebar(cx)),
            )
            .child(
                resizable_panel()
                    .size(px(layout.claude_width))
                    .size_range(px(320.)..Pixels::MAX)
                    .visible(claude_vis)
                    .child(div().size_full().children(claude)),
            )
            .child(
                resizable_panel()
                    .size_range(px(240.)..Pixels::MAX)
                    .visible(right_vis)
                    .child(self.area.clone()),
            );

        let body = div()
            .flex_1()
            .min_h_0()
            .flex()
            .when(self.sidebar_folded && self.focus_mode.is_none(), |d| {
                d.child(self.render_strip(cx))
            })
            .child(div().flex_1().min_w_0().h_full().child(columns));

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(bg)
            .text_color(fg)
            .key_context("Workspace")
            .track_focus(&self.focus)
            .on_action(cx.listener(|ws, _: &ToggleSidebar, _, cx| ws.toggle_sidebar(cx)))
            .on_action(
                cx.listener(|ws, _: &FocusRight, w, cx| ws.set_focus_mode(FocusMode::Right, w, cx)),
            )
            .on_action(
                cx.listener(|ws, _: &FocusClaude, w, cx| {
                    ws.set_focus_mode(FocusMode::Claude, w, cx)
                }),
            )
            .on_action(
                cx.listener(|ws, _: &Save, w, cx| ws.area.update(cx, |a, cx| a.save_active(w, cx))),
            )
            .on_action(
                cx.listener(|ws, _: &CloseTab, _, cx| {
                    ws.area.update(cx, |a, cx| a.close_active(cx))
                }),
            )
            .on_action(cx.listener(|ws, _: &TogglePreview, w, cx| {
                ws.area.update(cx, |a, cx| a.toggle_preview(w, cx))
            }))
            .on_action(cx.listener(|ws, _: &RenameTab, w, cx| {
                ws.area.update(cx, |a, cx| a.rename_active(w, cx))
            }))
            .on_action(
                cx.listener(|ws, _: &GoBack, w, cx| ws.area.update(cx, |a, cx| a.go_back(w, cx))),
            )
            .on_action(cx.listener(|ws, _: &GoForward, w, cx| {
                ws.area.update(cx, |a, cx| a.go_forward(w, cx))
            }))
            .on_action(cx.listener(|ws, _: &SplitRight, _, cx| {
                ws.area.update(cx, |a, cx| a.toggle_split(Split::Right, cx))
            }))
            .on_action(cx.listener(|ws, _: &SplitDown, _, cx| {
                ws.area.update(cx, |a, cx| a.toggle_split(Split::Down, cx))
            }))
            .on_action(cx.listener(|ws, _: &MoveTabToOtherHalf, w, cx| {
                ws.area.update(cx, |a, cx| {
                    if let Some(id) = a.active_id() {
                        let to = 1 - a.focused_pane;
                        a.move_to_pane(id, to, cx);
                        a.focus_active(w, cx);
                    }
                })
            }))
            .on_action(cx.listener(|ws, _: &NewSession, w, cx| ws.new_session(w, cx)))
            .on_action(
                cx.listener(|ws, _: &CloseSession, w, cx| {
                    ws.close_session(ws.active_session, w, cx)
                }),
            )
            .on_action(cx.listener(|ws, _: &NewTerminal, w, cx| ws.new_terminal(w, cx)))
            .on_action(cx.listener(|ws, _: &AskClaude, w, cx| {
                if ws.focus_mode == Some(FocusMode::Right) {
                    ws.focus_mode = None;
                }
                if let Some(s) = ws.active_session() {
                    s.panel.update(cx, |p, cx| p.focus_input(w, cx));
                }
                cx.notify();
            }))
            .child(top)
            .child(body)
    }
}
