//! One open file as a dock panel. Code opens in gpui-component's editor (tree-sitter
//! highlighting, line numbers, undo, find); markdown opens rendered with an instant switch to
//! the editor. Read and saved on background threads through the Host.
//!
//! A file the agent changed this session can also show as a diff: the whole file now, with the
//! lines the session added and removed marked (read-only, switched from the header).

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
use keleion_core::diff::{self, DiffRow, FileDiff, LineKind};
use keleion_core::files;
use keleion_core::host::Host;
use keleion_core::links::{self, FileIndex};
use keleion_core::symbols::SymbolIndex;

/// What every document needs from the window.
#[derive(Clone)]
pub struct DocContext {
    pub host: Arc<dyn Host>,
    pub root: PathBuf,
    pub index: Rc<RefCell<Arc<FileIndex>>>,
    pub symbols: Rc<RefCell<Arc<SymbolIndex>>>,
}

enum Body {
    Loading,
    Text(Entity<EditorState>),
    Message(String),
}

pub enum DocEvent {
    /// A link or a definition elsewhere: open it (through the window, for Back and Forward).
    OpenLink(PathBuf, Option<u32>),
    /// The title (dirty mark) changed.
    Changed,
    /// The tab was closed.
    Closed,
    /// Its tab became the shown one.
    Activated,
}

pub struct DocPanel {
    ctx: DocContext,
    pub path: PathBuf,
    body: Body,
    markdown: bool,
    /// Markdown shown rendered (else the editor).
    preview: bool,
    pub dirty: bool,
    pub read_only: bool,
    /// Text as last read or saved, to tell our own saves from outside changes.
    saved: String,
    pending_line: Option<u32>,
    opened_at: std::time::Instant,
    pub focus: FocusHandle,
    /// Another session in the same folder just edited this file (its name).
    edited_by: Option<String>,
    /// What this session changed in the file, oldest first (empty when it changed nothing).
    changes: Vec<FileDiff>,
    /// Showing what the session changed as a diff (else the file).
    diff_mode: bool,
    /// The diff once computed; `None` while it is being worked out.
    diff: Option<DiffView>,
    /// The line to show when the diff is ready (else the first change).
    diff_line: Option<u32>,
    _sub: Option<Subscription>,
}

/// The whole file as diff rows, and where each change starts.
struct DiffView {
    rows: Rc<Vec<DiffRow>>,
    starts: Vec<usize>,
    scroll: UniformListScrollHandle,
    /// The change last stepped to (an index into `starts`).
    at: usize,
}

impl EventEmitter<DocEvent> for DocPanel {}

impl DocPanel {
    pub fn new(
        ctx: DocContext,
        path: PathBuf,
        line: Option<u32>,
        read_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let markdown = files::is_markdown(&path);
        let mut this = Self {
            ctx,
            path,
            body: Body::Loading,
            markdown,
            preview: markdown && line.is_none(),
            dirty: false,
            read_only,
            saved: String::new(),
            pending_line: line,
            opened_at: std::time::Instant::now(),
            focus: cx.focus_handle(),
            edited_by: None,
            changes: Vec::new(),
            diff_mode: false,
            diff: None,
            diff_line: None,
            _sub: None,
        };
        this.load(window, cx);
        this
    }

    /// The automatic tab name; a SKILL.md is named after its skill folder.
    pub fn auto_name(&self) -> String {
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if name == "SKILL.md"
            && let Some(dir) = self.path.parent().and_then(|p| p.file_name())
        {
            return format!("{} (skill)", dir.to_string_lossy());
        }
        name
    }

    /// Another session sharing the folder just edited this file.
    pub fn edited_by_other(&mut self, by: String, cx: &mut Context<Self>) {
        self.edited_by = Some(by);
        cx.notify();
    }

    pub fn root(&self) -> PathBuf {
        self.ctx.root.clone()
    }

    pub fn relative(&self) -> String {
        self.path
            .strip_prefix(&self.ctx.root)
            .unwrap_or(&self.path)
            .display()
            .to_string()
    }

    /// The cursor line (1-based), for Back and Forward.
    pub fn cursor_line(&self, cx: &App) -> u32 {
        match &self.body {
            Body::Text(e) => e.read(cx).cursor_position().line + 1,
            _ => 1,
        }
    }

    pub fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let host = self.ctx.host.clone();
        let path = self.path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let read = cx.background_spawn(async move { host.read(&path) }).await;
            let _ = this.update_in(cx, |doc, window, cx| doc.loaded(read, window, cx));
        })
        .detach();
    }

    fn loaded(
        &mut self,
        read: std::io::Result<Vec<u8>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = match read {
            Err(e) => {
                self.body = Body::Message(format!("Could not read the file: {e}"));
                cx.notify();
                return;
            }
            Ok(bytes) if !files::looks_like_text(&bytes) => {
                self.body =
                    Body::Message(format!("Binary file, {} bytes. Not shown.", bytes.len()));
                cx.notify();
                return;
            }
            Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
        };
        // An open editor: replace its text (the file changed on disk).
        if let Body::Text(editor) = &self.body {
            let editor = editor.clone();
            let cursor = editor.read(cx).cursor_position();
            editor.update(cx, |e, cx| {
                e.set_value(text.clone(), window, cx);
                e.set_cursor_position(cursor, window, cx);
            });
            self.saved = text;
            self.dirty = false;
            self.compute_diff(window, cx);
            cx.emit(DocEvent::Changed);
            cx.notify();
            return;
        }
        let language = files::language_for(&self.path);
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language)
                .line_number(true)
                .soft_wrap(language == "markdown" || language == "text")
                .searchable(true)
                .default_value(text.clone())
        });
        let me = cx.entity().downgrade();
        let provider = Rc::new(SymbolDefinitions {
            symbols: self.ctx.symbols.clone(),
            path: self.path.clone(),
        });
        editor.update(cx, |e, _| {
            let lsp = e.lsp_mut();
            lsp.definition_provider = Some(provider);
            // Definitions open through the window, so they land in a tab and join the history.
            lsp.show_document = Some(Rc::new(
                move |params: &lsp_types::ShowDocumentParams, window: &mut Window, cx: &mut App| {
                    let Some(path) = path_from_uri(params.uri.as_str()) else {
                        return false;
                    };
                    let line = params.selection.map(|r| r.start.line + 1);
                    let me = me.clone();
                    window.defer(cx, move |_, cx| {
                        let _ = me.update(cx, |_, cx| cx.emit(DocEvent::OpenLink(path, line)));
                    });
                    true
                },
            ));
        });
        let sub = cx.subscribe(&editor, |doc, editor, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                let changed = doc.saved != editor.read(cx).value().as_ref();
                if doc.dirty != changed {
                    doc.dirty = changed;
                    cx.emit(DocEvent::Changed);
                }
                cx.notify();
            }
        });
        self.saved = text;
        self._sub = Some(sub);
        self.body = Body::Text(editor);
        if let Some(line) = self.pending_line.take() {
            self.go_to_line(line, window, cx);
        }
        self.compute_diff(window, cx);
        let started = self.opened_at;
        window.on_next_frame(move |_, _| {
            crate::perf::record("file open to first frame", started.elapsed())
        });
        cx.notify();
    }

    pub fn go_to_line(&mut self, line: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.preview = false;
        self.diff_mode = false;
        match &self.body {
            Body::Text(editor) => {
                let editor = editor.clone();
                editor.update(cx, |e, cx| {
                    e.set_cursor_position(Position::new(line.saturating_sub(1), 0), window, cx)
                });
            }
            _ => self.pending_line = Some(line),
        }
        cx.notify();
    }

    /// The file changed on disk: reload unless it has unsaved edits.
    pub fn file_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.dirty {
            self.load(window, cx);
        }
    }

    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            window.push_notification("This file belongs to a plugin and is read-only.", cx);
            return;
        }
        let Body::Text(editor) = &self.body else {
            return;
        };
        let text = editor.read(cx).value().to_string();
        let path = self.path.clone();
        let host = self.ctx.host.clone();
        let saved_text = text.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { host.write(&path, text.as_bytes()) })
                .await;
            let _ = this.update_in(cx, |doc, window, cx| match result {
                Ok(()) => {
                    doc.saved = saved_text;
                    doc.dirty = false;
                    cx.emit(DocEvent::Changed);
                    cx.notify();
                }
                Err(e) => window.push_notification(format!("Save failed: {e}"), cx),
            });
        })
        .detach();
    }

    /// What this session changed in the file (oldest first), for the diff. Without changes the
    /// diff switch goes away; an open diff is worked out again with the next read of the file.
    pub fn set_changes(&mut self, changes: Vec<FileDiff>, cx: &mut Context<Self>) {
        if changes.is_empty() {
            self.diff_mode = false;
        }
        self.changes = changes;
        cx.notify();
    }

    /// Show the file as a diff of what the session changed, at `line` of the file now (else at
    /// the first change).
    pub fn show_diff(&mut self, line: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        if self.changes.is_empty() {
            return;
        }
        self.diff_mode = true;
        self.diff_line = line;
        self.compute_diff(window, cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Switch between the diff and the file.
    pub fn toggle_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.diff_mode {
            self.diff_mode = false;
            self.focus_content(window, cx);
            cx.notify();
        } else {
            self.show_diff(None, window, cx);
        }
    }

    /// Work out the diff on a background thread, from the text in the editor.
    fn compute_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Body::Text(editor) = &self.body else {
            return;
        };
        if !self.diff_mode {
            return;
        }
        let text = editor.read(cx).value().to_string();
        let changes = self.changes.clone();
        cx.spawn_in(window, async move |this, cx| {
            let rows = cx
                .background_spawn(async move { diff::session_rows(&text, &changes) })
                .await;
            let _ = this.update(cx, |doc, cx| doc.diff_ready(rows, cx));
        })
        .detach();
    }

    fn diff_ready(&mut self, rows: Vec<DiffRow>, cx: &mut Context<Self>) {
        let starts = diff::change_starts(&rows);
        // Where to look: the asked-for line, else where it was, else the first change.
        let scroll = self.diff.as_ref().map(|d| d.scroll.clone());
        let target = match self.diff_line.take() {
            Some(line) => rows.iter().position(|r| r.new == Some(line as usize)),
            None if scroll.is_some() => None,
            None => starts.first().copied(),
        };
        let scroll = scroll.unwrap_or_default();
        if let Some(ix) = target {
            scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        let at = target
            .and_then(|ix| starts.iter().rposition(|&s| s <= ix))
            .unwrap_or(0);
        self.diff = Some(DiffView {
            rows: Rc::new(rows),
            starts,
            scroll,
            at,
        });
        cx.notify();
    }

    /// Step to the next change (`forward`) or the one before, wrapping around.
    fn step_change(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(view) = self.diff.as_mut() else {
            return;
        };
        let n = view.starts.len();
        if n == 0 {
            return;
        }
        view.at = if forward {
            (view.at + 1) % n
        } else {
            (view.at + n - 1) % n
        };
        view.scroll
            .scroll_to_item(view.starts[view.at], ScrollStrategy::Center);
        cx.notify();
    }

    /// Markdown: switch between rendered and editor.
    pub fn toggle_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.markdown {
            self.preview = !self.preview;
        }
        self.focus_content(window, cx);
        cx.notify();
    }

    /// Keyboard focus to the editor (or the panel itself for a rendered page).
    pub fn focus_content(&self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.body {
            Body::Text(editor) if !self.preview => {
                let editor = editor.clone();
                editor.update(cx, |e, cx| e.focus(window, cx));
            }
            _ => window.focus(&self.focus, cx),
        }
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        if self.diff_mode && matches!(self.body, Body::Text(_)) {
            return self.render_diff(cx);
        }
        match &self.body {
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
            Body::Text(editor) if self.markdown && self.preview => {
                let text = editor.read(cx).value();
                // Frontmatter shows as a YAML block rather than being misread as markdown.
                let text = match keleion_core::frontmatter::split(&text) {
                    Some((header, body)) => format!("```yaml\n{header}```\n\n{body}"),
                    None => text.to_string(),
                };
                let md = self.ctx.index.borrow().linkify_markdown(&text);
                let me = cx.entity().downgrade();
                let id = cx.entity().entity_id().as_u64() as usize;
                div()
                    .id(("md-preview", id))
                    .size_full()
                    .overflow_y_scrollbar()
                    .child(
                        div().px_4().py_2().max_w(px(860.)).child(
                            TextView::markdown(("md", id), md)
                                .selectable(true)
                                .on_link_click(move |url, _, _, cx| {
                                    if let Some((path, line)) = links::parse_url(url) {
                                        let _ = me.update(cx, |_, cx| {
                                            cx.emit(DocEvent::OpenLink(path, line))
                                        });
                                    } else {
                                        cx.open_url(url);
                                    }
                                }),
                        ),
                    )
                    .into_any_element()
            }
            Body::Text(editor) => Editor::new(editor)
                .h_full()
                .bordered(false)
                .readonly(self.read_only)
                .into_any_element(),
        }
    }

    /// The whole file with the session's changes marked, one row per line.
    fn render_diff(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, green, red) = (
            theme.foreground,
            theme.muted_foreground,
            theme.green,
            theme.red,
        );
        let font = theme.mono_font_family.clone();
        let Some(view) = &self.diff else {
            return div()
                .p_2()
                .text_xs()
                .text_color(muted)
                .child("Comparing...")
                .into_any_element();
        };
        if view.starts.is_empty() {
            return div()
                .p_2()
                .text_sm()
                .text_color(muted)
                .child("The file is as it was before this session changed it.")
                .into_any_element();
        }
        let rows = view.rows.clone();
        let last = rows
            .iter()
            .filter_map(|r| r.old.max(r.new))
            .max()
            .unwrap_or(1);
        // Room for the longest line number in each column.
        let number_w = px(8. + 7. * last.to_string().len() as f32);
        let id = cx.entity().entity_id().as_u64() as usize;
        uniform_list(("diff", id), rows.len(), move |range, _window, _cx| {
            range
                .map(|ix| {
                    let row = &rows[ix];
                    let (sign, bg, sign_fg) = match row.kind {
                        LineKind::Added => ("+", green.opacity(0.16), green),
                        LineKind::Removed => ("-", red.opacity(0.16), red),
                        LineKind::Context => (" ", transparent_black(), muted),
                    };
                    let number = |n: Option<usize>| {
                        div()
                            .w(number_w)
                            .flex_none()
                            .text_right()
                            .pr_1()
                            .text_color(muted.opacity(0.7))
                            .child(n.map(|n| n.to_string()).unwrap_or_default())
                    };
                    div()
                        .h(px(18.))
                        .flex()
                        .items_center()
                        .bg(bg)
                        .child(number(row.old))
                        .child(number(row.new))
                        .child(
                            div()
                                .w(px(16.))
                                .flex_none()
                                .text_center()
                                .text_color(sign_fg)
                                .child(sign),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_color(if row.kind == LineKind::Context {
                                    fg
                                } else {
                                    fg.opacity(0.95)
                                })
                                .child(row.text.replace('\t', "    ")),
                        )
                })
                .collect()
        })
        .track_scroll(&view.scroll)
        .size_full()
        .font_family(font)
        .text_size(px(12.))
        .into_any_element()
    }

    /// The path row above the content, with the markdown switch.
    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, border, hover) = (theme.muted_foreground, theme.border, theme.secondary_hover);
        let (warn, green, red) = (theme.warning, theme.green, theme.red);
        let preview = self.preview;
        div()
            .h(px(20.))
            .flex_none()
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(11.))
            .text_color(muted)
            .border_b_1()
            .border_color(border)
            .child(div().flex_1().truncate().child(self.relative()))
            .when_some(self.edited_by.clone(), |d, by| {
                d.child(
                    div()
                        .id("edited-by")
                        .px_1()
                        .rounded_sm()
                        .bg(warn.opacity(0.18))
                        .text_color(warn)
                        .cursor_pointer()
                        .child(format!("also edited by session \"{by}\"; click to dismiss"))
                        .on_click(cx.listener(|doc, _, _, cx| {
                            doc.edited_by = None;
                            cx.notify();
                        })),
                )
            })
            .when(self.read_only, |d| d.child("read-only"))
            .when(self.diff_mode, |d| {
                let changes = self.diff.as_ref().map_or(0, |v| v.starts.len());
                let (added, removed) = self.diff.as_ref().map_or((0, 0), |v| {
                    let count = |k| v.rows.iter().filter(|r| r.kind == k).count();
                    (count(LineKind::Added), count(LineKind::Removed))
                });
                let step = |id: &'static str, label: &'static str, forward: bool| {
                    div()
                        .id(id)
                        .px_1()
                        .rounded_sm()
                        .cursor_pointer()
                        .hover(|d| d.bg(hover))
                        .child(label)
                        .on_click(cx.listener(move |doc, _, _, cx| doc.step_change(forward, cx)))
                };
                d.child(div().text_color(green).child(format!("+{added}")))
                    .child(div().text_color(red).child(format!("-{removed}")))
                    .when(changes > 1, |d| {
                        d.child(step("diff-prev", "Previous", false)).child(step(
                            "diff-next",
                            "Next change",
                            true,
                        ))
                    })
            })
            .when(!self.changes.is_empty(), |d| {
                d.child(
                    div()
                        .id("diff-toggle")
                        .px_1()
                        .rounded_sm()
                        .cursor_pointer()
                        .when(self.diff_mode, |d| d.bg(hover))
                        .hover(|d| d.bg(hover))
                        .child(if self.diff_mode { "File" } else { "Diff" })
                        .on_click(cx.listener(|doc, _, window, cx| doc.toggle_diff(window, cx))),
                )
            })
            .when(self.markdown && !self.diff_mode, |d| {
                d.child(
                    div()
                        .id("md-toggle")
                        .px_1()
                        .rounded_sm()
                        .cursor_pointer()
                        .hover(|d| d.bg(hover))
                        .child(if preview {
                            "Edit (Cmd+E)"
                        } else {
                            "Preview (Cmd+E)"
                        })
                        .on_click(cx.listener(|doc, _, window, cx| doc.toggle_preview(window, cx))),
                )
            })
            .into_any_element()
    }
}

impl Focusable for DocPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DocPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let focused = self.focus.contains_focused(window, cx);
        let (bg, accent) = (theme.background, theme.ring);
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(bg)
            .track_focus(&self.focus)
            .key_context("Doc")
            .border_t_2()
            .border_color(if focused { accent } else { transparent_black() })
            .child(self.render_header(cx))
            .child(div().flex_1().min_h_0().child(self.render_body(cx)))
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
