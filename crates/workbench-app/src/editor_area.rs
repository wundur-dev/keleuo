//! The centre: open files as tabs. Code opens in gpui-component's editor (tree-sitter
//! highlighting, line numbers, undo, find); markdown opens rendered with an instant switch to
//! the editor and back. Files are read and saved on background threads through the Host.

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
use workbench_core::tabs::NameStore;

use crate::tab_strip::{TabSpec, TabStrip, TabStripEvent};

enum Body {
    Loading,
    Text(Entity<EditorState>),
    Message(String),
}

struct Doc {
    id: u64,
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

pub enum EditorAreaEvent {
    /// The active file changed (to reveal it in the tree).
    Activated(PathBuf),
    OpenLink(PathBuf, Option<u32>),
}

pub struct EditorArea {
    host: Arc<dyn Host>,
    root: PathBuf,
    docs: Vec<Doc>,
    active: Option<u64>,
    next_id: u64,
    pub strip: Entity<TabStrip>,
    pub index: Arc<FileIndex>,
    focus: FocusHandle,
    /// Definitions across the project, filled in the background.
    pub symbols: Rc<RefCell<Arc<SymbolIndex>>>,
    /// Where jumps (links, go to definition) came from, for Back and Forward.
    back: Vec<(PathBuf, u32)>,
    forward: Vec<(PathBuf, u32)>,
    _strip_sub: Subscription,
}

impl EventEmitter<EditorAreaEvent> for EditorArea {}

impl EditorArea {
    pub fn new(
        host: Arc<dyn Host>,
        root: PathBuf,
        names: Rc<RefCell<NameStore>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let strip = cx.new(|_| TabStrip::new("editor-tabs", names));
        let sub = cx.subscribe(&strip, |area, _, ev: &TabStripEvent, cx| match ev {
            TabStripEvent::Activate(id) => area.activate(*id, cx),
            TabStripEvent::Close(id) => area.close(*id, cx),
        });
        Self {
            host,
            root,
            docs: Vec::new(),
            active: None,
            next_id: 1,
            strip,
            index: Arc::new(FileIndex::default()),
            focus: cx.focus_handle(),
            symbols: Rc::new(RefCell::new(Arc::new(SymbolIndex::default()))),
            back: Vec::new(),
            forward: Vec::new(),
            _strip_sub: sub,
        }
    }

    fn sync_strip(&mut self, cx: &mut Context<Self>) {
        let tabs = self
            .docs
            .iter()
            .map(|d| TabSpec {
                id: d.id,
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
            })
            .collect();
        let active = self.active;
        self.strip.update(cx, |s, cx| s.set_tabs(tabs, active, cx));
    }

    fn doc(&self, id: u64) -> Option<&Doc> {
        self.docs.iter().find(|d| d.id == id)
    }

    fn doc_mut(&mut self, id: u64) -> Option<&mut Doc> {
        self.docs.iter_mut().find(|d| d.id == id)
    }

    fn activate(&mut self, id: u64, cx: &mut Context<Self>) {
        self.active = Some(id);
        if let Some(path) = self.doc(id).map(|d| d.path.clone()) {
            cx.emit(EditorAreaEvent::Activated(path));
        }
        self.sync_strip(cx);
        cx.notify();
    }

    pub fn close(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(ix) = self.docs.iter().position(|d| d.id == id) else {
            return;
        };
        self.docs.remove(ix);
        if self.active == Some(id) {
            self.active = self
                .docs
                .get(ix.min(self.docs.len().saturating_sub(1)))
                .map(|d| d.id);
        }
        self.sync_strip(cx);
        cx.notify();
    }

    pub fn close_active(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.active {
            self.close(id, cx);
        }
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
            self.activate(id, cx);
            self.focus_active(window, cx);
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        let markdown = files::is_markdown(&path);
        self.docs.push(Doc {
            id,
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
        self.activate(id, cx);
        window.focus(&self.focus, cx);
        self.load(id, window, cx);
    }

    /// The active file and its cursor line (1-based).
    fn current_location(&self, cx: &App) -> Option<(PathBuf, u32)> {
        let doc = self.doc(self.active?)?;
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

    /// Keyboard focus to the active file: its editor, or the area itself for a preview.
    pub fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active.and_then(|id| self.doc(id)) {
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
            self.sync_strip(cx);
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
                    area.sync_strip(cx);
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
        if self.active == Some(id) {
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
        let Some(id) = self.active else { return };
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
                    area.sync_strip(cx);
                    cx.notify();
                }
                Err(e) => window.push_notification(format!("Save failed: {e}"), cx),
            });
        })
        .detach();
    }

    /// Markdown: switch between rendered and editor.
    pub fn toggle_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active else { return };
        if let Some(doc) = self.doc_mut(id)
            && doc.markdown
        {
            doc.preview = !doc.preview;
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    pub fn rename_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.active {
            self.strip
                .update(cx, |s, cx| s.start_rename(id, window, cx));
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
                                                cx.emit(EditorAreaEvent::OpenLink(path, line))
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

impl Render for EditorArea {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let base = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .track_focus(&self.focus);
        let Some(doc) = self.active.and_then(|id| self.doc(id)) else {
            return base
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Open a file from the tree, a skill from the left, or a link in the chat.");
        };
        let header = self.render_header(doc, cx);
        let body = self.render_doc(doc, cx);
        base.child(header)
            .child(div().flex_1().min_h_0().child(body))
    }
}
