//! The right area: one tab row for files, skills being edited and terminals. It can split in
//! two (side by side or one above the other) to show two tabs at once; unsplit by default.
//!
//! Code opens in gpui-component's editor (tree-sitter highlighting, line numbers, undo, find);
//! markdown opens rendered with an instant switch to the editor. Files are read and saved on
//! background threads through the Host.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::input::{Editor, EditorState, InputEvent, Position};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::*;
use workbench_core::files;
use workbench_core::host::Host;
use workbench_core::links::{self, FileIndex};
use workbench_core::symbols::SymbolIndex;
use workbench_core::tabs::{NameStore, auto_name};

use crate::tab_strip::{DraggedTab, TabSpec, TabStrip, TabStripEvent};
use crate::terminal_view::TerminalView;

enum Body {
    Loading,
    Text(Entity<EditorState>),
    Message(String),
}

struct Doc {
    id: u64,
    pane: usize,
    path: PathBuf,
    body: Body,
    markdown: bool,
    /// Markdown shown rendered (else the editor).
    preview: bool,
    dirty: bool,
    read_only: bool,
    /// Text as last read or saved, to tell our own saves from outside changes.
    saved: String,
    pending_line: Option<u32>,
    opened_at: std::time::Instant,
    _sub: Option<Subscription>,
}

pub struct Term {
    pub id: u64,
    pane: usize,
    /// 1, 2, ... in opening order; the key of its saved name.
    pub number: usize,
    pub view: Entity<TerminalView>,
    _obs: Subscription,
}

/// How the area is split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Split {
    Right,
    Down,
}

pub enum RightAreaEvent {
    /// The active file changed (to reveal it in the tree).
    Activated(PathBuf),
    OpenLink(PathBuf, Option<u32>),
}

/// A terminal as the sidebar lists it.
pub struct TermInfo {
    pub id: u64,
    pub name: String,
    pub key: String,
    pub unseen: bool,
    pub shown: bool,
}

pub struct RightArea {
    host: Arc<dyn Host>,
    root: PathBuf,
    names: Rc<RefCell<NameStore>>,
    docs: Vec<Doc>,
    terms: Vec<Term>,
    /// Tab ids in display order (both panes).
    order: Vec<u64>,
    active: [Option<u64>; 2],
    /// The pane keyboard actions go to.
    pub focused_pane: usize,
    pub split: Option<Split>,
    next_id: u64,
    next_term: usize,
    strips: [Entity<TabStrip>; 2],
    pub index: Arc<FileIndex>,
    focus: FocusHandle,
    /// Definitions across the project, filled in the background.
    pub symbols: Rc<RefCell<Arc<SymbolIndex>>>,
    /// Where jumps (links, go to definition) came from, for Back and Forward.
    back: Vec<(PathBuf, u32)>,
    forward: Vec<(PathBuf, u32)>,
    branch: Option<String>,
    shell: String,
    _subs: Vec<Subscription>,
}

impl EventEmitter<RightAreaEvent> for RightArea {}

impl RightArea {
    pub fn new(
        host: Arc<dyn Host>,
        root: PathBuf,
        names: Rc<RefCell<NameStore>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let strips = [0usize, 1]
            .map(|pane| cx.new(|_| TabStrip::new(&format!("tabs-{pane}"), names.clone())));
        let subs = strips
            .iter()
            .map(|strip| {
                cx.subscribe(strip, |area, _, ev: &TabStripEvent, cx| match ev {
                    TabStripEvent::Activate(id) => area.activate(*id, cx),
                    TabStripEvent::Close(id) => area.close(*id, cx),
                })
            })
            .collect();
        let mut this = Self {
            host,
            root,
            names,
            docs: Vec::new(),
            terms: Vec::new(),
            order: Vec::new(),
            active: [None, None],
            focused_pane: 0,
            split: None,
            next_id: 1,
            next_term: 1,
            strips,
            index: Arc::new(FileIndex::default()),
            focus: cx.focus_handle(),
            symbols: Rc::new(RefCell::new(Arc::new(SymbolIndex::default()))),
            back: Vec::new(),
            forward: Vec::new(),
            branch: None,
            shell: std::env::var("SHELL").unwrap_or_default(),
            _subs: subs,
        };
        this.load_branch(cx);
        this
    }

    fn load_branch(&mut self, cx: &mut Context<Self>) {
        let host = self.host.clone();
        let root = self.root.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let branch = cx
                .background_spawn(async move { host.git_branch(&root) })
                .await;
            let _ = this.update(cx, |a, cx| {
                a.branch = branch;
                a.sync_strips(cx);
            });
        })
        .detach();
    }

    // ----- tabs and panes -----

    fn pane_of(&self, id: u64) -> Option<usize> {
        self.docs
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.pane)
            .or_else(|| self.terms.iter().find(|t| t.id == id).map(|t| t.pane))
    }

    fn set_pane(&mut self, id: u64, pane: usize) {
        if let Some(d) = self.docs.iter_mut().find(|d| d.id == id) {
            d.pane = pane;
        }
        if let Some(t) = self.terms.iter_mut().find(|t| t.id == id) {
            t.pane = pane;
        }
    }

    fn tabs_in(&self, pane: usize) -> Vec<u64> {
        self.order
            .iter()
            .copied()
            .filter(|id| self.pane_of(*id) == Some(pane))
            .collect()
    }

    fn term_key(&self, number: usize) -> String {
        NameStore::key(&self.root, &format!("terminal:{number}"))
    }

    fn term_auto_name(&self, t: &Term, cx: &App) -> String {
        let view = t.view.read(cx);
        let program = view.pty.as_ref().and_then(|p| p.foreground_program());
        let mut name = auto_name(
            &self.root,
            self.branch.as_deref(),
            program.as_deref(),
            &self.shell,
        );
        if self.terms.len() > 1 && program.as_deref().is_none_or(|p| p.contains("sh")) {
            name = format!("{name} {}", t.number);
        }
        if view.exited {
            name.push_str(" (exited)");
        }
        name
    }

    fn spec_for(&self, id: u64, cx: &App) -> Option<TabSpec> {
        if let Some(d) = self.docs.iter().find(|d| d.id == id) {
            return Some(TabSpec {
                id,
                key: NameStore::key(&self.root, &format!("file:{}", d.path.display())),
                auto_name: tab_name(&d.path),
                dirty: d.dirty,
                tooltip: Some(
                    d.path
                        .strip_prefix(&self.root)
                        .unwrap_or(&d.path)
                        .display()
                        .to_string(),
                ),
                terminal: false,
            });
        }
        let t = self.terms.iter().find(|t| t.id == id)?;
        Some(TabSpec {
            id,
            key: self.term_key(t.number),
            auto_name: self.term_auto_name(t, cx),
            dirty: false,
            tooltip: t.view.read(cx).title.clone(),
            terminal: true,
        })
    }

    fn sync_strips(&mut self, cx: &mut Context<Self>) {
        for pane in 0..2 {
            let tabs: Vec<TabSpec> = self
                .tabs_in(pane)
                .into_iter()
                .filter_map(|id| self.spec_for(id, cx))
                .collect();
            let active = self.active[pane];
            let changed = {
                let s = self.strips[pane].read(cx);
                s.active != active
                    || s.tabs.len() != tabs.len()
                    || s.tabs.iter().zip(&tabs).any(|(a, b)| {
                        a.id != b.id
                            || a.auto_name != b.auto_name
                            || a.dirty != b.dirty
                            || a.tooltip != b.tooltip
                    })
            };
            if changed {
                self.strips[pane].update(cx, |s, cx| s.set_tabs(tabs, active, cx));
            }
        }
        cx.notify();
    }

    pub fn activate(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(pane) = self.pane_of(id) else { return };
        self.active[pane] = Some(id);
        self.focused_pane = pane;
        if let Some(path) = self
            .docs
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.path.clone())
        {
            cx.emit(RightAreaEvent::Activated(path));
        }
        if let Some(t) = self.terms.iter().find(|t| t.id == id) {
            t.view.update(cx, |v, _| v.unseen = false);
        }
        self.sync_strips(cx);
    }

    /// Activate and give keyboard focus.
    pub fn show(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.activate(id, cx);
        self.focus_active(window, cx);
    }

    pub fn close(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(pane) = self.pane_of(id) else { return };
        let siblings = self.tabs_in(pane);
        let ix = siblings.iter().position(|t| *t == id).unwrap_or(0);
        self.docs.retain(|d| d.id != id);
        self.terms.retain(|t| t.id != id);
        self.order.retain(|t| *t != id);
        if self.active[pane] == Some(id) {
            let left: Vec<u64> = siblings.into_iter().filter(|t| *t != id).collect();
            self.active[pane] = left.get(ix.min(left.len().saturating_sub(1))).copied();
        }
        // An empty second pane closes the split.
        if self.split.is_some() && self.tabs_in(1).is_empty() {
            self.unsplit(cx);
        } else if self.split.is_some() && self.tabs_in(0).is_empty() {
            for t in self.tabs_in(1) {
                self.set_pane(t, 0);
            }
            self.active = [self.active[1], None];
            self.split = None;
            self.focused_pane = 0;
        }
        self.sync_strips(cx);
    }

    pub fn close_active(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.active[self.focused_pane] {
            self.close(id, cx);
        }
    }

    /// Split the area; the active tab of the focused pane moves to the new half when there is
    /// another tab to leave behind.
    pub fn split(&mut self, how: Split, cx: &mut Context<Self>) {
        if self.split.is_some() {
            self.split = Some(how);
            cx.notify();
            return;
        }
        self.split = Some(how);
        if self.tabs_in(0).len() > 1
            && let Some(id) = self.active[0]
        {
            self.move_to_pane(id, 1, cx);
        } else {
            self.focused_pane = 1;
        }
        self.sync_strips(cx);
    }

    pub fn unsplit(&mut self, cx: &mut Context<Self>) {
        for t in self.tabs_in(1) {
            self.set_pane(t, 0);
        }
        if self.active[0].is_none() {
            self.active[0] = self.active[1];
        }
        self.active[1] = None;
        self.split = None;
        self.focused_pane = 0;
        self.sync_strips(cx);
    }

    pub fn toggle_split(&mut self, how: Split, cx: &mut Context<Self>) {
        if self.split == Some(how) {
            self.unsplit(cx)
        } else {
            self.split(how, cx)
        }
    }

    /// Move a tab to a pane (splitting when needed).
    pub fn move_to_pane(&mut self, id: u64, pane: usize, cx: &mut Context<Self>) {
        let Some(from) = self.pane_of(id) else { return };
        if from == pane {
            self.activate(id, cx);
            return;
        }
        if pane == 1 && self.split.is_none() {
            self.split = Some(Split::Right);
        }
        let left: Vec<u64> = self
            .tabs_in(from)
            .into_iter()
            .filter(|t| *t != id)
            .collect();
        self.set_pane(id, pane);
        if self.active[from] == Some(id) {
            self.active[from] = left.first().copied();
        }
        self.active[pane] = Some(id);
        self.focused_pane = pane;
        if self.split.is_some()
            && (self.tabs_in(0).is_empty() || self.tabs_in(1).is_empty())
            && from == 1
        {
            self.unsplit(cx);
            self.activate(id, cx);
            return;
        }
        if self.tabs_in(0).is_empty() {
            // Everything went right: keep one pane.
            for t in self.tabs_in(1) {
                self.set_pane(t, 0);
            }
            self.active = [Some(id), None];
            self.split = None;
            self.focused_pane = 0;
        }
        self.sync_strips(cx);
    }

    /// A tab dropped on the right or bottom half of the area.
    fn dropped(&mut self, tab: &DraggedTab, how: Split, cx: &mut Context<Self>) {
        if self.split.is_none() {
            if self.tabs_in(0).len() < 2 {
                return;
            }
            self.split = Some(how);
        }
        self.move_to_pane(tab.id, 1, cx);
    }

    /// The tab the focused pane shows.
    pub fn active_id(&self) -> Option<u64> {
        self.active[self.focused_pane]
    }

    // ----- terminals -----

    pub fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) -> u64 {
        let spec = self.host.shell_command(&self.root);
        let view = cx.new(|cx| TerminalView::new(spec, cx));
        // Re-derive the automatic name when the terminal redraws (the program changes).
        let obs = cx.observe(&view, |area, view, cx| {
            let id = area.terms.iter().find(|t| t.view == view).map(|t| t.id);
            let shown = id.is_some_and(|id| area.active.contains(&Some(id)));
            if shown {
                view.update(cx, |v, _| v.unseen = false);
            }
            area.sync_strips(cx);
        });
        let id = self.next_id;
        self.next_id += 1;
        let number = self.next_term;
        self.next_term += 1;
        self.terms.push(Term {
            id,
            pane: self.focused_pane,
            number,
            view,
            _obs: obs,
        });
        self.order.push(id);
        self.show(id, window, cx);
        id
    }

    pub fn terminals(&self, cx: &App) -> Vec<TermInfo> {
        self.terms
            .iter()
            .map(|t| {
                let key = self.term_key(t.number);
                let name = self
                    .names
                    .borrow()
                    .get(&key)
                    .map(str::to_string)
                    .unwrap_or_else(|| self.term_auto_name(t, cx));
                TermInfo {
                    id: t.id,
                    name,
                    key,
                    unseen: t.view.read(cx).unseen,
                    shown: self.active.contains(&Some(t.id)),
                }
            })
            .collect()
    }

    /// Write to the shown terminal (control channel).
    pub fn write_terminal(&self, text: &str, cx: &App) {
        let id = self.active[self.focused_pane];
        let term = self
            .terms
            .iter()
            .find(|t| Some(t.id) == id)
            .or(self.terms.last());
        if let Some(pty) = term.and_then(|t| t.view.read(cx).pty.clone()) {
            pty.write(text.as_bytes().to_vec());
        }
    }

    // ----- files -----

    fn doc(&self, id: u64) -> Option<&Doc> {
        self.docs.iter().find(|d| d.id == id)
    }

    fn doc_mut(&mut self, id: u64) -> Option<&mut Doc> {
        self.docs.iter_mut().find(|d| d.id == id)
    }

    /// Open `path` (or switch to it), at `line` when given.
    pub fn open(
        &mut self,
        path: PathBuf,
        line: Option<u32>,
        read_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.docs.iter().find(|d| d.path == path).map(|d| d.id) {
            if let Some(line) = line {
                self.go_to_line(id, line, window, cx);
            }
            self.show(id, window, cx);
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        let markdown = files::is_markdown(&path);
        // Files open in the focused pane, unless it shows a terminal and the other pane does not.
        let mut pane = self.focused_pane;
        if self.split.is_some()
            && self.active[pane].is_some_and(|a| self.terms.iter().any(|t| t.id == a))
        {
            pane = 1 - pane;
        }
        self.docs.push(Doc {
            id,
            pane,
            path: path.clone(),
            body: Body::Loading,
            markdown,
            preview: markdown && line.is_none(),
            dirty: false,
            read_only,
            saved: String::new(),
            pending_line: line,
            opened_at: std::time::Instant::now(),
            _sub: None,
        });
        self.order.push(id);
        self.activate(id, cx);
        window.focus(&self.focus, cx);
        self.load(id, window, cx);
    }

    /// The active file and its cursor line (1-based).
    fn current_location(&self, cx: &App) -> Option<(PathBuf, u32)> {
        let doc = self.doc(self.active_id()?)?;
        let line = match &doc.body {
            Body::Text(e) => e.read(cx).cursor_position().line + 1,
            _ => 1,
        };
        Some((doc.path.clone(), line))
    }

    /// Open a link or definition target, remembering where we were.
    pub fn jump(
        &mut self,
        path: PathBuf,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(here) = self.current_location(cx) {
            self.back.push(here);
            self.forward.clear();
        }
        self.open(path, line, false, window, cx);
    }

    pub fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, line)) = self.back.pop() {
            if let Some(here) = self.current_location(cx) {
                self.forward.push(here);
            }
            self.open(path, Some(line), false, window, cx);
        }
    }

    pub fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, line)) = self.forward.pop() {
            if let Some(here) = self.current_location(cx) {
                self.back.push(here);
            }
            self.open(path, Some(line), false, window, cx);
        }
    }

    /// Keyboard focus to the focused pane's tab: an editor, a terminal, or the area itself.
    pub fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.active_id();
        if let Some(t) = self.terms.iter().find(|t| Some(t.id) == id) {
            let handle = t.view.read(cx).focus.clone();
            window.focus(&handle, cx);
            return;
        }
        match id.and_then(|id| self.doc(id)) {
            Some(Doc {
                body: Body::Text(editor),
                preview: false,
                ..
            }) => {
                let editor = editor.clone();
                editor.update(cx, |e, cx| e.focus(window, cx));
            }
            _ => window.focus(&self.focus, cx),
        }
    }

    fn load(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.doc(id).map(|d| d.path.clone()) else {
            return;
        };
        let host = self.host.clone();
        cx.spawn_in(window, async move |this, cx| {
            let read = cx.background_spawn(async move { host.read(&path) }).await;
            let _ = this.update_in(cx, |area, window, cx| area.loaded(id, read, window, cx));
        })
        .detach();
    }

    fn loaded(
        &mut self,
        id: u64,
        read: std::io::Result<Vec<u8>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(doc) = self.docs.iter().find(|d| d.id == id) else {
            return;
        };
        let text = match read {
            Err(e) => {
                self.set_body(
                    id,
                    Body::Message(format!("Could not read the file: {e}")),
                    cx,
                );
                return;
            }
            Ok(bytes) if !files::looks_like_text(&bytes) => {
                self.set_body(
                    id,
                    Body::Message(format!("Binary file, {} bytes. Not shown.", bytes.len())),
                    cx,
                );
                return;
            }
            Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
        };
        // An open editor: replace its text (Claude edited the file).
        if let Body::Text(editor) = &doc.body {
            let editor = editor.clone();
            let cursor = editor.read(cx).cursor_position();
            editor.update(cx, |e, cx| {
                e.set_value(text.clone(), window, cx);
                e.set_cursor_position(cursor, window, cx);
            });
            if let Some(doc) = self.doc_mut(id) {
                doc.saved = text;
                doc.dirty = false;
            }
            self.sync_strips(cx);
            cx.notify();
            return;
        }
        let language = files::language_for(&doc.path);
        let read_only = doc.read_only;
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language)
                .line_number(true)
                .soft_wrap(language == "markdown" || language == "text")
                .searchable(true)
                .default_value(text.clone())
        });
        let _ = read_only;
        let area = cx.entity().downgrade();
        let provider = Rc::new(SymbolDefinitions {
            symbols: self.symbols.clone(),
            path: doc.path.clone(),
        });
        editor.update(cx, |e, _| {
            let lsp = e.lsp_mut();
            lsp.definition_provider = Some(provider);
            // Every definition target (this file or another) opens through the area, so it
            // lands in a tab and joins the Back and Forward history.
            lsp.show_document = Some(Rc::new(
                move |params: &lsp_types::ShowDocumentParams, window: &mut Window, cx: &mut App| {
                    let Some(path) = path_from_uri(params.uri.as_str()) else {
                        return false;
                    };
                    let line = params.selection.map(|r| r.start.line + 1);
                    let area = area.clone();
                    window.defer(cx, move |window, cx| {
                        let _ = area.update(cx, |a, cx| a.jump(path, line, window, cx));
                    });
                    true
                },
            ));
        });
        let sub = cx.subscribe(&editor, move |area, editor, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                let changed = {
                    let value = editor.read(cx).value();
                    area.doc(id).is_some_and(|d| d.saved != value.as_ref())
                };
                if let Some(doc) = area.doc_mut(id)
                    && doc.dirty != changed
                {
                    doc.dirty = changed;
                    area.sync_strips(cx);
                }
                cx.notify();
            }
        });
        let line = if let Some(doc) = self.doc_mut(id) {
            doc.saved = text;
            doc._sub = Some(sub);
            doc.body = Body::Text(editor);
            doc.pending_line.take()
        } else {
            None
        };
        if let Some(line) = line {
            self.go_to_line(id, line, window, cx);
        }
        if self.active_id() == Some(id) {
            self.focus_active(window, cx);
        }
        if let Some(doc) = self.doc(id) {
            let started = doc.opened_at;
            window.on_next_frame(move |_, _| {
                crate::perf::record("file open to first frame", started.elapsed())
            });
        }
        cx.notify();
    }

    fn set_body(&mut self, id: u64, body: Body, cx: &mut Context<Self>) {
        if let Some(doc) = self.doc_mut(id) {
            doc.body = body;
        }
        cx.notify();
    }

    fn go_to_line(&mut self, id: u64, line: u32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.doc_mut(id) else { return };
        doc.preview = false;
        match &doc.body {
            Body::Text(editor) => {
                let editor = editor.clone();
                editor.update(cx, |e, cx| {
                    e.set_cursor_position(Position::new(line.saturating_sub(1), 0), window, cx)
                });
            }
            _ => doc.pending_line = Some(line),
        }
        cx.notify();
    }

    /// Claude (or something else) changed a file on disk: reload it unless it has unsaved edits.
    pub fn file_changed(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<u64> = self
            .docs
            .iter()
            .filter(|d| d.path == path && !d.dirty)
            .map(|d| d.id)
            .collect();
        for id in ids {
            self.load(id, window, cx);
        }
    }

    pub fn save_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_id() else { return };
        let Some(doc) = self.doc(id) else { return };
        if doc.read_only {
            window.push_notification("This file belongs to a plugin and is read-only.", cx);
            return;
        }
        let Body::Text(editor) = &doc.body else {
            return;
        };
        let text = editor.read(cx).value().to_string();
        let path = doc.path.clone();
        let host = self.host.clone();
        let saved_text = text.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { host.write(&path, text.as_bytes()) })
                .await;
            let _ = this.update_in(cx, |area, window, cx| match result {
                Ok(()) => {
                    if let Some(doc) = area.doc_mut(id) {
                        doc.saved = saved_text;
                        doc.dirty = false;
                    }
                    area.sync_strips(cx);
                    cx.notify();
                }
                Err(e) => window.push_notification(format!("Save failed: {e}"), cx),
            });
        })
        .detach();
    }

    /// Markdown: switch between rendered and editor.
    pub fn toggle_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_id() else { return };
        if let Some(doc) = self.doc_mut(id)
            && doc.markdown
        {
            doc.preview = !doc.preview;
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    pub fn rename_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.active_id() {
            self.rename_tab(id, window, cx);
        }
    }

    fn render_doc(&self, doc: &Doc, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        match &doc.body {
            Body::Loading => div()
                .p_2()
                .text_xs()
                .text_color(muted)
                .child("Opening...")
                .into_any_element(),
            Body::Message(m) => div()
                .p_2()
                .text_sm()
                .text_color(muted)
                .child(m.clone())
                .into_any_element(),
            Body::Text(editor) => {
                if doc.markdown && doc.preview {
                    let text = editor.read(cx).value();
                    // Show frontmatter as a YAML block rather than letting markdown misread it.
                    let text = match workbench_core::frontmatter::split(&text) {
                        Some((header, body)) => format!("```yaml\n{}```\n\n{body}", header),
                        None => text.to_string(),
                    };
                    let md = self.index.linkify_markdown(&text);
                    let entity = cx.entity().downgrade();
                    div()
                        .id(("md-preview", doc.id as usize))
                        .size_full()
                        .overflow_y_scrollbar()
                        .child(
                            div().px_4().py_2().max_w(px(860.)).child(
                                TextView::markdown(("md", doc.id as usize), md)
                                    .selectable(true)
                                    .on_link_click(move |url, _, _, cx| {
                                        if let Some((path, line)) = links::parse_url(url) {
                                            let _ = entity.update(cx, |_, cx| {
                                                cx.emit(RightAreaEvent::OpenLink(path, line))
                                            });
                                        } else {
                                            cx.open_url(url);
                                        }
                                    }),
                            ),
                        )
                        .into_any_element()
                } else {
                    Editor::new(editor)
                        .h_full()
                        .bordered(false)
                        .readonly(doc.read_only)
                        .into_any_element()
                }
            }
        }
    }

    /// The small row above the editor: path and the markdown switch.
    fn render_header(&self, doc: &Doc, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let rel = doc
            .path
            .strip_prefix(&self.root)
            .unwrap_or(&doc.path)
            .display()
            .to_string();
        let mut row = div()
            .h(px(20.))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(11.))
            .text_color(theme.muted_foreground)
            .border_b_1()
            .border_color(theme.border)
            .child(div().flex_1().truncate().child(rel));
        if doc.read_only {
            row = row.child("read-only");
        }
        if doc.markdown {
            let preview = doc.preview;
            row = row.child(
                div()
                    .id("md-toggle")
                    .px_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(|d| d.bg(theme.secondary_hover))
                    .child(if preview {
                        "Edit (Cmd+E)"
                    } else {
                        "Preview (Cmd+E)"
                    })
                    .on_click(cx.listener(|area, _, window, cx| area.toggle_preview(window, cx))),
            );
        }
        row.into_any_element()
    }

    pub fn rename_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.pane_of(id) {
            self.strips[pane].update(cx, |s, cx| s.start_rename(id, window, cx));
        }
    }

    fn render_tab(&self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        if let Some(t) = self.terms.iter().find(|t| t.id == id) {
            return t.view.clone().into_any_element();
        }
        match self.doc(id) {
            Some(doc) => {
                let header = self.render_header(doc, cx);
                let body = self.render_doc(doc, cx);
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(header)
                    .child(div().flex_1().min_h_0().child(body))
                    .into_any_element()
            }
            None => div().into_any_element(),
        }
    }

    fn render_pane(&self, pane: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (border, muted, tab_bar, accent) = (
            theme.border,
            theme.muted_foreground,
            theme.tab_bar,
            theme.ring,
        );
        let focused = self.split.is_some() && self.focused_pane == pane;
        let body = match self.active[pane] {
            Some(id) => self.render_tab(id, cx),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(muted)
                .child(if pane == 0 {
                    "Files, skills and terminals open here. Cmd+T for a terminal."
                } else {
                    "Drag a tab here, or open something."
                })
                .into_any_element(),
        };
        div()
            .id(("pane", pane))
            .size_full()
            .flex()
            .flex_col()
            .min_w_0()
            .min_h_0()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |area, _, _, cx| {
                    if area.focused_pane != pane {
                        area.focused_pane = pane;
                        cx.notify();
                    }
                }),
            )
            .child(
                div()
                    .h(px(26.))
                    .flex_none()
                    .flex()
                    .border_b_1()
                    .border_color(if focused { accent } else { border })
                    .bg(tab_bar)
                    .child(self.strips[pane].clone()),
            )
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    /// Drop targets shown while a tab is dragged: the right half and the bottom half.
    fn render_drop_zones(&self, cx: &mut Context<Self>) -> AnyElement {
        let hl = cx.theme().ring.opacity(0.25);
        let zone = |id: &'static str, how: Split| {
            div()
                .id(id)
                .absolute()
                .bg(transparent_black())
                .drag_over::<DraggedTab>(move |s, _, _, _| s.bg(hl))
                .on_drop(cx.listener(move |area, tab: &DraggedTab, window, cx| {
                    area.dropped(tab, how, cx);
                    area.focus_active(window, cx);
                }))
        };
        div()
            .absolute()
            .size_full()
            .top_0()
            .left_0()
            .child(
                zone("drop-right", Split::Right)
                    .top(px(26.))
                    .right_0()
                    .w(relative(0.4))
                    .bottom_0(),
            )
            .child(
                zone("drop-down", Split::Down)
                    .bottom_0()
                    .left_0()
                    .w(relative(0.6))
                    .h(relative(0.4)),
            )
            .into_any_element()
    }
}

impl Render for RightArea {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, border) = (theme.background, theme.border);
        let dragging = cx.has_active_drag();
        let content = match self.split {
            None => self.render_pane(0, cx),
            Some(how) => {
                let a = self.render_pane(0, cx);
                let b = self.render_pane(1, cx);
                let group = match how {
                    Split::Right => gpui_kit::component::h_resizable("right-split"),
                    Split::Down => gpui_kit::component::v_resizable("right-split"),
                };
                let _ = border;
                group
                    .child(
                        gpui_kit::component::resizable_panel()
                            .size_range(px(160.)..Pixels::MAX)
                            .child(a),
                    )
                    .child(
                        gpui_kit::component::resizable_panel()
                            .size_range(px(160.)..Pixels::MAX)
                            .child(b),
                    )
                    .into_any_element()
            }
        };
        div()
            .relative()
            .size_full()
            .bg(bg)
            .track_focus(&self.focus)
            .child(content)
            .when(dragging, |d| d.child(self.render_drop_zones(cx)))
    }
}

/// Go to definition from the project symbol index (Cmd+click or F12 in the editor).
struct SymbolDefinitions {
    symbols: Rc<RefCell<Arc<SymbolIndex>>>,
    path: PathBuf,
}

impl gpui_kit::component::input::DefinitionProvider for SymbolDefinitions {
    fn definitions(
        &self,
        text: &gpui_kit::component::input::Rope,
        offset: usize,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Task<anyhow::Result<Vec<lsp_types::LocationLink>>> {
        use gpui_kit::component::input::RopeExt;
        let Some(range) = text.word_range(offset) else {
            return Task::ready(Ok(Vec::new()));
        };
        let word = text.word_at(offset);
        let index = self.symbols.borrow().clone();
        let start = text.offset_to_position(range.start);
        let end = text.offset_to_position(range.end);
        let links = index
            .lookup(&word, Some(&self.path))
            .into_iter()
            // Clicking the definition itself goes nowhere.
            .filter(|s| !(s.path == self.path && s.line == start.line + 1))
            .take(1)
            .filter_map(|s| {
                let uri = uri_from_path(&s.path)?;
                let pos = Position::new(s.line - 1, s.column);
                let target = lsp_types::Range::new(
                    pos,
                    Position::new(s.line - 1, s.column + s.name.len() as u32),
                );
                Some(lsp_types::LocationLink {
                    origin_selection_range: Some(lsp_types::Range::new(start, end)),
                    target_uri: uri,
                    target_range: target,
                    target_selection_range: target,
                })
            })
            .collect();
        Task::ready(Ok(links))
    }
}

fn uri_from_path(path: &Path) -> Option<lsp_types::Uri> {
    let encoded: String = path
        .to_string_lossy()
        .chars()
        .map(|c| match c {
            ' ' => "%20".to_string(),
            '%' => "%25".to_string(),
            '#' => "%23".to_string(),
            '?' => "%3F".to_string(),
            c => c.to_string(),
        })
        .collect();
    format!("file://{encoded}").parse().ok()
}

fn path_from_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    Some(PathBuf::from(
        rest.replace("%20", " ")
            .replace("%23", "#")
            .replace("%3F", "?")
            .replace("%25", "%"),
    ))
}

/// A file's automatic tab name; a SKILL.md is named after its skill folder.
fn tab_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if name == "SKILL.md"
        && let Some(dir) = path.parent().and_then(|p| p.file_name())
    {
        return format!("{} (skill)", dir.to_string_lossy());
    }
    name
}
