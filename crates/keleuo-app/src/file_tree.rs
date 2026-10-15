//! The project file tree: compact rows in a virtual list, folders expand on click. Listing runs
//! on a background thread through the Host.

use std::collections::HashSet;
use crate::panels::{LIST_TEXT, ROW_HEIGHT};
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleuo_core::files::{self, Node};
use keleuo_core::host::Host;
use keleuo_core::links::FileIndex;

pub enum FileTreeEvent {
    Open(PathBuf),
    /// A file or folder was renamed: from, to (open tabs follow).
    Renamed(PathBuf, PathBuf),
    /// A new file list (for link resolution).
    Indexed(Arc<FileIndex>),
}

struct Row {
    depth: usize,
    name: String,
    path: PathBuf,
    is_dir: bool,
    open: bool,
    kind: RowKind,
}

#[derive(Clone, Copy, PartialEq)]
enum RowKind {
    Entry,
    /// The name field of a file or folder being made.
    Name,
    /// Why that name did not work.
    Problem,
}

/// A new file or folder being named, in place in the tree.
struct Creating {
    /// The folder it goes in.
    dir: PathBuf,
    folder: bool,
    input: Entity<InputState>,
    problem: Option<String>,
    _sub: Subscription,
}

/// A file or folder being renamed, its row a name field.
struct Renaming {
    path: PathBuf,
    input: Entity<InputState>,
    problem: Option<String>,
    _sub: Subscription,
}

pub struct FileTree {
    focus: FocusHandle,
    host: Arc<dyn Host>,
    root: PathBuf,
    nodes: Vec<Node>,
    expanded: HashSet<PathBuf>,
    rows: Arc<Vec<Row>>,
    pub selected: Option<PathBuf>,
    loading: bool,
    creating: Option<Creating>,
    renaming: Option<Renaming>,
    /// Select this path once the next listing comes in (one just made).
    reveal_after: Option<PathBuf>,
    /// Every path listed, to tell a new file from a changed one.
    known: HashSet<PathBuf>,
    /// Paths that changed while not listed, waiting for the listing they asked for.
    asked: Vec<PathBuf>,
    /// Changed paths the listing leaves out (ignored by git): no re-listing for them again.
    unlisted: HashSet<PathBuf>,
    /// The row under the pointer: a right-click there makes new files in its folder, one on
    /// empty space at the project root.
    hovered: Option<usize>,
    scroll: UniformListScrollHandle,
    _task: Option<Task<()>>,
}

impl EventEmitter<FileTreeEvent> for FileTree {}

impl FileTree {
    pub fn new(host: Arc<dyn Host>, root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            focus: cx.focus_handle(),
            host,
            root,
            nodes: Vec::new(),
            expanded: HashSet::new(),
            rows: Arc::new(Vec::new()),
            selected: None,
            loading: true,
            creating: None,
            renaming: None,
            reveal_after: None,
            known: HashSet::new(),
            asked: Vec::new(),
            unlisted: HashSet::new(),
            hovered: None,
            scroll: UniformListScrollHandle::new(),
            _task: None,
        };
        this.refresh(cx);
        this
    }

    /// Re-list the project in the background.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let host = self.host.clone();
        let root = self.root.clone();
        self._task = Some(
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let (nodes, index, known) = cx
                    .background_spawn(async move {
                        let list = host.list_files(&root, files::MAX_ENTRIES);
                        let index = FileIndex::new(&root, &list);
                        let known: HashSet<PathBuf> = list.iter().map(|(rel, _)| root.join(rel)).collect();
                        (files::tree_from_list(&root, list), index, known)
                    })
                    .await;
                let _ = this.update(cx, |tree, cx| {
                    tree.nodes = nodes;
                    for p in std::mem::take(&mut tree.asked) {
                        if !known.contains(&p) {
                            tree.unlisted.insert(p);
                        }
                    }
                    tree.known = known;
                    tree.loading = false;
                    tree.rebuild_rows();
                    if let Some(path) = tree.reveal_after.take() {
                        tree.reveal(&path, cx);
                    }
                    cx.emit(FileTreeEvent::Indexed(Arc::new(index)));
                    cx.notify();
                });
            }),
        );
    }

    fn rebuild_rows(&mut self) {
        fn walk(nodes: &[Node], depth: usize, expanded: &HashSet<PathBuf>, out: &mut Vec<Row>) {
            for n in nodes {
                out.push(Row {
                    depth,
                    name: n.name.clone(),
                    path: n.path.clone(),
                    is_dir: n.is_dir(),
                    open: expanded.contains(&n.path),
                    kind: RowKind::Entry,
                });
                if let Some(children) = &n.children
                    && expanded.contains(&n.path)
                {
                    walk(children, depth + 1, expanded, out);
                }
            }
        }
        let mut rows = Vec::new();
        walk(&self.nodes, 0, &self.expanded, &mut rows);
        // The name field goes first in its folder.
        if let Some(c) = &self.creating {
            let (at, depth) = match rows.iter().position(|r| r.path == c.dir) {
                Some(ix) => (ix + 1, rows[ix].depth + 1),
                None => (0, 0),
            };
            let row = |kind| Row { depth, name: String::new(), path: c.dir.clone(), is_dir: c.folder, open: false, kind };
            if c.problem.is_some() {
                rows.insert(at, row(RowKind::Problem));
            }
            rows.insert(at, row(RowKind::Name));
        }
        self.rows = Arc::new(rows);
    }

    /// Whether these changed paths change the tree: one gone, or one there but not listed
    /// (new). A path still left out after the listing it asked for is not asked about again.
    pub fn changes_listing(&mut self, paths: &[PathBuf]) -> bool {
        let mut changes = false;
        for p in paths {
            let listed = self.known.contains(p);
            if listed != p.exists() && !self.unlisted.contains(p) {
                changes = true;
                if !listed {
                    self.asked.push(p.clone());
                }
            }
        }
        changes
    }

    /// Where a right-click makes new files: the hovered folder, a hovered file's folder, else
    /// the project root.
    fn menu_folder(&self) -> PathBuf {
        match self.hovered.and_then(|ix| self.rows.get(ix)).filter(|r| r.kind == RowKind::Entry) {
            Some(r) if r.is_dir => r.path.clone(),
            Some(r) => r.path.parent().map_or_else(|| self.root.clone(), |p| p.to_path_buf()),
            None => self.root.clone(),
        }
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// Start making a new file (or folder) in `dir`: a name field opens there in the tree.
    pub fn start_create(&mut self, dir: PathBuf, folder: bool, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = if folder { "Folder name" } else { "File name (a/b.rs makes folders)" };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        input.update(cx, |s, cx| s.focus(window, cx));
        let sub = cx.subscribe_in(&input, window, |tree, input, ev: &InputEvent, _, cx| match ev {
            InputEvent::PressEnter { .. } => tree.finish_create(cx),
            InputEvent::Change => {
                if let Some(c) = tree.creating.as_mut().filter(|c| c.problem.is_some()) {
                    c.problem = None;
                    tree.rebuild_rows();
                    cx.notify();
                }
            }
            // Clicking away keeps a name typed, and drops an empty field.
            InputEvent::Blur if input.read(cx).value().trim().is_empty() => tree.cancel_create(cx),
            _ => {}
        });
        if dir != self.root {
            self.expanded.insert(dir.clone());
        }
        self.creating = Some(Creating { dir, folder, input, problem: None, _sub: sub });
        self.rebuild_rows();
        if let Some(ix) = self.rows.iter().position(|r| r.kind == RowKind::Name) {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        cx.notify();
    }

    fn cancel_create(&mut self, cx: &mut Context<Self>) {
        if self.creating.take().is_some() {
            self.rebuild_rows();
            cx.notify();
        }
    }

    /// Make what the name field names; a new file opens.
    fn finish_create(&mut self, cx: &mut Context<Self>) {
        let Some(c) = &self.creating else { return };
        let (dir, folder) = (c.dir.clone(), c.folder);
        let name = c.input.read(cx).value().to_string();
        let host = self.host.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let made = cx.background_spawn(async move { files::create(host.as_ref(), &dir, &name, folder) }).await;
            let _ = this.update(cx, |tree, cx| match made {
                Ok(path) => {
                    tree.creating = None;
                    if folder {
                        tree.expanded.insert(path.clone());
                    } else {
                        tree.selected = Some(path.clone());
                        cx.emit(FileTreeEvent::Open(path.clone()));
                    }
                    tree.reveal_after = Some(path);
                    tree.refresh(cx);
                }
                Err(problem) => {
                    if let Some(c) = tree.creating.as_mut() {
                        c.problem = Some(problem);
                    }
                    tree.rebuild_rows();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Rename `path`: its row becomes a name field (Enter renames, Esc or clicking away keeps it).
    pub fn start_rename(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        input.update(cx, |s, cx| s.focus(window, cx));
        let sub = cx.subscribe_in(&input, window, |tree, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::PressEnter { .. } => tree.finish_rename(cx),
            InputEvent::Blur => tree.cancel_rename(cx),
            _ => {}
        });
        self.renaming = Some(Renaming { path, input, problem: None, _sub: sub });
        cx.notify();
    }

    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        if self.renaming.take().is_some() {
            cx.notify();
        }
    }

    fn finish_rename(&mut self, cx: &mut Context<Self>) {
        let Some(r) = &self.renaming else { return };
        let (from, name) = (r.path.clone(), r.input.read(cx).value().to_string());
        let host = self.host.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let to = { let from = from.clone(); cx.background_spawn(async move { files::rename(host.as_ref(), &from, &name) }).await };
            let _ = this.update(cx, |tree, cx| match to {
                Ok(to) => {
                    tree.renaming = None;
                    if to != from {
                        if tree.expanded.remove(&from) {
                            tree.expanded.insert(to.clone());
                        }
                        tree.selected = Some(to.clone());
                        cx.emit(FileTreeEvent::Renamed(from, to.clone()));
                        tree.reveal_after = Some(to);
                        tree.refresh(cx);
                    }
                    cx.notify();
                }
                Err(problem) => {
                    if let Some(r) = tree.renaming.as_mut() {
                        r.problem = Some(problem);
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Take `path` away once the person says so: into the Trash (deleted over SSH, said first).
    pub fn discard(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let what = if self.host.is_file(&path) { "file" } else { "folder" };
        let detail = if self.host.is_remote() {
            format!("Deletes it on {} (there is no Trash there).", self.host.label())
        } else {
            "Moves it to the Trash.".to_string()
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Remove the {what} {name}?"),
            Some(&detail),
            &[PromptButton::Ok("Remove".into()), PromptButton::Cancel("Cancel".into())],
            cx,
        );
        let host = self.host.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            if answer.await.ok() != Some(0) {
                return;
            }
            let gone = { let path = path.clone(); cx.background_spawn(async move { files::discard(host.as_ref(), &path) }).await };
            let _ = this.update(cx, |tree, cx| {
                if let Err(e) = gone {
                    eprintln!("keleuo: could not remove {}: {e}", path.display());
                }
                if tree.selected.as_ref().is_some_and(|s| s.starts_with(&path)) {
                    tree.selected = None;
                }
                tree.refresh(cx);
            });
        })
        .detach();
    }

    fn click(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix) else { return };
        let path = row.path.clone();
        if row.is_dir {
            if !self.expanded.remove(&path) {
                self.expanded.insert(path);
            }
            self.rebuild_rows();
        } else {
            self.selected = Some(path.clone());
            cx.emit(FileTreeEvent::Open(path));
        }
        cx.notify();
    }

    /// Expand the folders above `path` and select it.
    pub fn reveal(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let mut p = path.parent();
        while let Some(dir) = p {
            if dir == self.root {
                break;
            }
            self.expanded.insert(dir.to_path_buf());
            p = dir.parent();
        }
        self.selected = Some(path.to_path_buf());
        self.rebuild_rows();
        if let Some(ix) = self.rows.iter().position(|r| r.path == path) {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        cx.notify();
    }
}

/// "New File…" and "New Folder…", made in `dir`.
fn new_menu(menu: PopupMenu, tree: WeakEntity<FileTree>, dir: PathBuf) -> PopupMenu {
    let item = |label: &'static str, icon, folder: bool| {
        let (tree, dir) = (tree.clone(), dir.clone());
        PopupMenuItem::new(label).icon(Icon::new(icon)).on_click(move |_, window, cx| {
            let _ = tree.update(cx, |t, cx| t.start_create(dir.clone(), folder, window, cx));
        })
    };
    menu.item(item("New File…", gpui_kit::assets::IconName::FilePlus, false))
        .item(item("New Folder…", gpui_kit::assets::IconName::FolderPlus, true))
}

impl Focusable for FileTree {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for FileTree {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, muted, sel_bg, hover) = (
            theme.foreground,
            theme.muted_foreground,
            theme.accent,
            theme.secondary_hover,
        );
        let quiet = theme.sidebar_foreground.opacity(0.88);
        if self.loading {
            return div()
                .p_2()
                .text_size(px(LIST_TEXT))
                .text_color(muted)
                .child("Listing files...")
                .into_any_element();
        }
        let rows = self.rows.clone();
        let selected = self.selected.clone();
        let entity = cx.entity().downgrade();
        let menu_tree = entity.clone();
        let sidebar_bg = crate::theme::tool_ground(cx);
        let danger = cx.theme().danger;
        let naming = self.creating.as_ref().map(|c| (c.input.clone(), c.problem.clone().unwrap_or_default()));
        let renaming = self.renaming.as_ref().map(|r| (r.path.clone(), r.input.clone(), r.problem.clone()));
        let list = uniform_list("file-tree", rows.len(), move |range, _window, cx| {
            range
                .map(|ix| {
                    let row = &rows[ix];
                    let entity = entity.clone();
                    let indent = px(6. + row.depth as f32 * 12.);
                    if let (RowKind::Name | RowKind::Problem, Some((input, problem))) = (row.kind, &naming) {
                        let base = div().id(ix).w_full().h(px(ROW_HEIGHT)).pl(indent).pr_1().flex().items_center().gap_1();
                        let el = if row.kind == RowKind::Name {
                            let icon = if row.is_dir { crate::icons::folder(false, cx) } else { crate::icons::file("", cx) };
                            base.child(icon).child(
                                div()
                                    .flex_1()
                                    .min_w(px(80.))
                                    .capture_key_down(move |ev: &KeyDownEvent, _, cx| {
                                        if ev.keystroke.key == "escape" {
                                            cx.stop_propagation();
                                            let _ = entity.update(cx, |tree, cx| tree.cancel_create(cx));
                                        }
                                    })
                                    .child(Input::new(input).xsmall()),
                            )
                        } else {
                            base.text_size(px(LIST_TEXT)).text_color(danger).child(div().truncate().child(problem.clone()))
                        };
                        return el.into_any_element();
                    }
                    let is_sel = selected.as_ref() == Some(&row.path);
                    let hover_tree = entity.clone();
                    let icon = if row.is_dir {
                        crate::icons::folder(row.open, cx)
                    } else {
                        crate::icons::file(&row.name, cx)
                    };
                    // Being renamed: a name field in its place.
                    if let Some((_, input, problem)) = renaming.as_ref().filter(|(p, _, _)| *p == row.path) {
                        let cancel = entity.clone();
                        return div()
                            .id(ix)
                            .h(px(ROW_HEIGHT))
                            .pl(indent)
                            .pr_1()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(icon)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(80.))
                                    .capture_key_down(move |ev: &KeyDownEvent, _, cx| {
                                        if ev.keystroke.key == "escape" {
                                            cx.stop_propagation();
                                            let _ = cancel.update(cx, |tree, cx| tree.cancel_rename(cx));
                                        }
                                    })
                                    .child(Input::new(input).xsmall()),
                            )
                            .children(problem.clone().map(|p| div().flex_none().max_w(px(140.)).truncate().text_size(px(11.)).text_color(danger).child(p)))
                            .into_any_element();
                    }
                    // Rename and remove, on hover (right-click is easy to miss on a Mac).
                    let group = SharedString::from(format!("file-row-{ix}"));
                    let (ren, rem) = (entity.clone(), entity.clone());
                    let (ren_path, rem_path) = (row.path.clone(), row.path.clone());
                    let icons = div()
                        .flex_none()
                        .flex()
                        .invisible()
                        .group_hover(group.clone(), |d| d.visible())
                        .child(
                            Button::new(("file-rename", ix))
                                .icon(gpui_kit::assets::IconName::Pencil)
                                .xsmall()
                                .ghost()
                                .tooltip("Rename")
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    let _ = ren.update(cx, |t, cx| t.start_rename(ren_path.clone(), window, cx));
                                }),
                        )
                        .child(
                            Button::new(("file-remove", ix))
                                .icon(gpui_kit::assets::IconName::Trash)
                                .xsmall()
                                .ghost()
                                .tooltip("Remove")
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    let _ = rem.update(cx, |t, cx| t.discard(rem_path.clone(), window, cx));
                                }),
                        );
                    div()
                        .id(ix)
                        .group(group)
                        .h(px(ROW_HEIGHT))
                        .pl(indent)
                        .pr_1()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_size(px(LIST_TEXT))
                        .text_color(if row.is_dir || is_sel { fg } else { quiet })
                        .when(is_sel, |d| d.bg(sel_bg))
                        .hover(|d| d.bg(hover))
                        .child(icon)
                        .child(div().flex_1().min_w_0().truncate().child(row.name.clone()))
                        .child(icons)
                        .on_click(move |_, _, cx| {
                            let _ = entity.update(cx, |tree, cx| tree.click(ix, cx));
                        })
                        .on_hover(move |inside, _, cx| {
                            let _ = hover_tree.update(cx, |t, _| {
                                if *inside {
                                    t.hovered = Some(ix);
                                } else if t.hovered == Some(ix) {
                                    t.hovered = None;
                                }
                            });
                        })
                        .into_any_element()
                })
                .collect()
        })
        .track_scroll(&self.scroll)
        .size_full();
        div()
            .id("file-tree-space")
            .size_full()
            .bg(sidebar_bg)
            .child(list)
            .context_menu(move |menu, _, cx| {
                let Some(tree) = menu_tree.upgrade() else { return menu };
                let dir = tree.read(cx).menu_folder();
                new_menu(menu, menu_tree.clone(), dir)
            })
            .into_any_element()
    }
}
