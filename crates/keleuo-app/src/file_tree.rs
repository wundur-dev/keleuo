//! The project file tree: compact rows in a virtual list, folders expand on click. Listing runs
//! on a background thread through the Host.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::ActiveTheme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleuo_core::files::{self, Node};
use keleuo_core::host::Host;
use keleuo_core::links::FileIndex;

pub enum FileTreeEvent {
    Open(PathBuf),
    /// A new file list (for link resolution).
    Indexed(Arc<FileIndex>),
}

struct Row {
    depth: usize,
    name: String,
    path: PathBuf,
    is_dir: bool,
    open: bool,
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
                let (nodes, index) = cx
                    .background_spawn(async move {
                        let list = host.list_files(&root, files::MAX_ENTRIES);
                        let index = FileIndex::new(&root, &list);
                        (files::tree_from_list(&root, list), index)
                    })
                    .await;
                let _ = this.update(cx, |tree, cx| {
                    tree.nodes = nodes;
                    tree.loading = false;
                    tree.rebuild_rows();
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
        self.rows = Arc::new(rows);
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
                .text_xs()
                .text_color(muted)
                .child("Listing files...")
                .into_any_element();
        }
        let rows = self.rows.clone();
        let selected = self.selected.clone();
        let entity = cx.entity().downgrade();
        let sidebar_bg = cx.theme().sidebar;
        let list = uniform_list("file-tree", rows.len(), move |range, _window, cx| {
            range
                .map(|ix| {
                    let row = &rows[ix];
                    let is_sel = selected.as_ref() == Some(&row.path);
                    let entity = entity.clone();
                    let icon = if row.is_dir {
                        crate::icons::folder(row.open, cx)
                    } else {
                        crate::icons::file(&row.name, cx)
                    };
                    div()
                        .id(ix)
                        .h(px(20.))
                        .pl(px(6. + row.depth as f32 * 12.))
                        .pr_1()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_xs()
                        .text_color(if row.is_dir || is_sel { fg } else { quiet })
                        .when(is_sel, |d| d.bg(sel_bg))
                        .hover(|d| d.bg(hover))
                        .child(icon)
                        .child(div().truncate().child(row.name.clone()))
                        .on_click(move |_, _, cx| {
                            let _ = entity.update(cx, |tree, cx| tree.click(ix, cx));
                        })
                })
                .collect()
        })
        .track_scroll(&self.scroll)
        .size_full();
        div()
            .size_full()
            .bg(sidebar_bg)
            .child(list)
            .into_any_element()
    }
}
