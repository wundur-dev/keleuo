//! The window: one thin top bar (panel switches, the editor tabs, status), then skills | files |
//! code | Claude side by side over the terminal. Every panel collapses with a shortcut and comes
//! back at its size; focus mode shows only the editor or only Claude.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, TitleBar, h_resizable, resizable_panel, v_resizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use workbench_core::host::Host;
use workbench_core::skills::Source;
use workbench_core::tabs::NameStore;

use crate::actions::*;
use crate::claude_panel::{ClaudePanel, ClaudePanelEvent};
use crate::claude_session::{ClaudeSession, SessionEvent};
use crate::editor_area::{EditorArea, EditorAreaEvent};
use crate::file_tree::{FileTree, FileTreeEvent};
use crate::skills_panel::{SkillsEvent, SkillsPanel};
use crate::terminal_panel::TerminalPanel;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FocusMode {
    Editor,
    Claude,
}

pub struct Workspace {
    host: Arc<dyn Host>,
    root: PathBuf,
    branch: Option<String>,
    skills: Entity<SkillsPanel>,
    tree: Entity<FileTree>,
    editor: Entity<EditorArea>,
    claude: Entity<ClaudePanel>,
    terminal: Entity<TerminalPanel>,
    show_skills: bool,
    show_files: bool,
    show_claude: bool,
    show_terminal: bool,
    focus_mode: Option<FocusMode>,
    first_frame_logged: bool,
    _watch: Option<workbench_core::host::Watch>,
    _subs: Vec<Subscription>,
}

impl Workspace {
    pub fn new(host: Arc<dyn Host>, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let names = Rc::new(RefCell::new(NameStore::open_default()));
        let skills = cx.new(|cx| SkillsPanel::new(host.clone(), root.clone(), cx));
        let tree = cx.new(|cx| FileTree::new(host.clone(), root.clone(), cx));
        let editor = cx.new(|cx| EditorArea::new(host.clone(), root.clone(), names.clone(), cx));
        let session = cx.new(|_| ClaudeSession::new(host.clone(), root.clone()));
        let claude = cx.new(|cx| ClaudePanel::new(session.clone(), window, cx));
        let terminal = cx.new(|cx| TerminalPanel::new(host.clone(), root.clone(), names.clone(), cx));

        let mut subs = Vec::new();
        subs.push(cx.subscribe_in(&skills, window, |ws, _, ev: &SkillsEvent, window, cx| match ev {
            SkillsEvent::Open(entry) => {
                let read_only = matches!(entry.source, Source::Plugin(_));
                let path = entry.path.clone();
                ws.editor.update(cx, |e, cx| e.open(path, None, read_only, window, cx));
            }
        }));
        subs.push(cx.subscribe_in(&tree, window, |ws, _, ev: &FileTreeEvent, window, cx| match ev {
            FileTreeEvent::Open(path) => {
                let path = path.clone();
                ws.editor.update(cx, |e, cx| e.open(path, None, false, window, cx));
            }
            FileTreeEvent::Indexed(index) => {
                ws.editor.update(cx, |e, _| e.index = index.clone());
                ws.claude.update(cx, |c, cx| {
                    c.index = index.clone();
                    cx.notify();
                });
            }
        }));
        subs.push(cx.subscribe_in(&editor, window, |ws, _, ev: &EditorAreaEvent, window, cx| match ev {
            EditorAreaEvent::Activated(path) => {
                let path = path.clone();
                if path.starts_with(&ws.root) {
                    ws.tree.update(cx, |t, cx| t.reveal(&path, cx));
                }
            }
            EditorAreaEvent::OpenLink(path, line) => ws.open_file(path.clone(), *line, window, cx),
        }));
        subs.push(cx.subscribe_in(&claude, window, |ws, _, ev: &ClaudePanelEvent, window, cx| match ev {
            ClaudePanelEvent::OpenFile(path, line) => ws.open_file(path.clone(), *line, window, cx),
        }));
        subs.push(cx.subscribe_in(&session, window, |ws, _, ev: &SessionEvent, window, cx| match ev {
            SessionEvent::FileChanged(path) => {
                let path = path.clone();
                ws.editor.update(cx, |e, cx| e.file_changed(&path, window, cx));
                if path.ends_with("SKILL.md") || path.components().any(|c| c.as_os_str() == ".claude") {
                    ws.skills.update(cx, |s, cx| s.refresh(None, cx));
                }
            }
            SessionEvent::TurnEnded => ws.tree.update(cx, |t, cx| t.refresh(cx)),
        }));

        // Outside changes (terminal, other editors): reload open files, re-list the tree.
        let (tx, rx) = std::sync::mpsc::channel();
        let watch = host.watch(&root, tx);
        if watch.is_some() {
            let (atx, arx) = async_channel::unbounded::<Vec<PathBuf>>();
            std::thread::spawn(move || {
                let noise = |p: &PathBuf| {
                    p.components().any(|c| matches!(c.as_os_str().to_str(), Some("target" | "node_modules" | ".git")))
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
                            ws.editor.update(cx, |e, cx| e.file_changed(p, window, cx));
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
        }

        let mut this = Self {
            host,
            root,
            branch: None,
            skills,
            tree,
            editor,
            claude,
            terminal,
            show_skills: true,
            show_files: true,
            show_claude: true,
            show_terminal: true,
            focus_mode: None,
            first_frame_logged: false,
            _watch: watch,
            _subs: subs,
        };
        this.load_branch(cx);
        this.terminal.update(cx, |t, cx| t.focus(window, cx));
        this
    }

    fn load_branch(&mut self, cx: &mut Context<Self>) {
        let host = self.host.clone();
        let root = self.root.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let branch = cx.background_spawn(async move { host.git_branch(&root) }).await;
            let _ = this.update(cx, |ws, cx| {
                ws.branch = branch;
                cx.notify();
            });
        })
        .detach();
    }

    fn open_file(&mut self, path: PathBuf, line: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus_mode == Some(FocusMode::Claude) {
            self.focus_mode = None;
        }
        self.editor.update(cx, |e, cx| e.open(path, line, false, window, cx));
        cx.notify();
    }

    fn visible(&self) -> (bool, bool, bool, bool, bool) {
        // (skills, files, editor, claude, terminal)
        match self.focus_mode {
            Some(FocusMode::Editor) => (false, false, true, false, false),
            Some(FocusMode::Claude) => (false, false, false, true, false),
            None => (self.show_skills, self.show_files, true, self.show_claude, self.show_terminal),
        }
    }

    fn toggle(&mut self, which: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_mode = None;
        match which {
            "skills" => self.show_skills = !self.show_skills,
            "files" => self.show_files = !self.show_files,
            "claude" => {
                self.show_claude = !self.show_claude;
                if self.show_claude {
                    self.claude.update(cx, |c, cx| c.focus_input(window, cx));
                }
            }
            "terminal" => {
                self.show_terminal = !self.show_terminal;
                if self.show_terminal {
                    self.terminal.update(cx, |t, cx| t.focus(window, cx));
                }
            }
            _ => {}
        }
        cx.notify();
    }

    fn set_focus_mode(&mut self, mode: FocusMode, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_mode = if self.focus_mode == Some(mode) { None } else { Some(mode) };
        if self.focus_mode == Some(FocusMode::Claude) {
            self.claude.update(cx, |c, cx| c.focus_input(window, cx));
        }
        cx.notify();
    }

    fn rename_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminal.read(cx).contains_focus(window, cx) {
            self.terminal.update(cx, |t, cx| t.rename_active(window, cx));
        } else {
            self.editor.update(cx, |e, cx| e.rename_active(window, cx));
        }
    }

    fn switch_button(&self, id: &'static str, icon: IconName, on: bool, tip: &'static str, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme();
        let (fg, muted, hover) = (theme.foreground, theme.muted_foreground, theme.secondary_hover);
        div()
            .id(id)
            .size(px(22.))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded_sm()
            .hover(|d| d.bg(hover))
            .child(Icon::new(icon).small().text_color(if on { fg } else { muted.opacity(0.6) }))
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip).build(window, cx))
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.first_frame_logged {
            self.first_frame_logged = true;
            window.on_next_frame(|_, _| {
                if let Some(start) = crate::START.get() {
                    eprintln!("workbench: first frame {:.0} ms after start", start.elapsed().as_secs_f64() * 1000.0);
                }
            });
        }
        let theme = cx.theme();
        let (bg, border, muted, bar_bg) = (theme.background, theme.border, theme.muted_foreground, theme.title_bar);
        let (warning, fg) = (theme.warning, theme.foreground);
        let (s_vis, f_vis, e_vis, c_vis, t_vis) = self.visible();
        let folder = self.root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let mut place = folder.clone();
        if let Some(b) = &self.branch {
            place.push_str(&format!(" ({b})"));
        }
        if self.host.is_remote() {
            place = format!("{}: {place}", self.host.label());
        }
        let focus_label = match self.focus_mode {
            Some(FocusMode::Editor) => Some("focus: editor"),
            Some(FocusMode::Claude) => Some("focus: Claude"),
            None => None,
        };

        let top = TitleBar::new()
            .h(px(30.))
            .pl(px(76.))
            .child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(self.switch_button("t-skills", IconName::BookOpen, s_vis, "Skills (Cmd+1)", cx).on_click(
                        cx.listener(|ws, _, w, cx| ws.toggle("skills", w, cx)),
                    ))
                    .child(self.switch_button("t-files", IconName::FolderOpen, f_vis, "Files (Cmd+2)", cx).on_click(
                        cx.listener(|ws, _, w, cx| ws.toggle("files", w, cx)),
                    ))
                    .child(div().h_full().flex_1().min_w_0().border_l_1().border_color(border).child(self.editor.read(cx).strip.clone()))
                    .child(div().flex_none().px_1().text_size(px(11.)).text_color(muted).child(place))
                    .when_some(focus_label, |d, l| d.child(div().flex_none().px_1().text_size(px(11.)).text_color(warning).child(l)))
                    .child(self.switch_button("t-term", IconName::PanelBottom, t_vis, "Terminal (Cmd+J)", cx).on_click(
                        cx.listener(|ws, _, w, cx| ws.toggle("terminal", w, cx)),
                    ))
                    .child(self.switch_button("t-claude", IconName::PanelRight, c_vis, "Claude (Cmd+3)", cx).on_click(
                        cx.listener(|ws, _, w, cx| ws.toggle("claude", w, cx)),
                    ))
                    .child(div().w(px(4.))),
            );

        let side = |child: AnyElement| div().size_full().bg(bar_bg).child(child);
        let main_row = h_resizable("main-h")
            .child(
                resizable_panel()
                    .size(px(230.))
                    .size_range(px(140.)..px(500.))
                    .visible(s_vis)
                    .child(side(self.skills.clone().into_any_element())),
            )
            .child(
                resizable_panel()
                    .size(px(210.))
                    .size_range(px(120.)..px(500.))
                    .visible(f_vis)
                    .child(side(self.tree.clone().into_any_element())),
            )
            .child(resizable_panel().visible(e_vis).size_range(px(200.)..Pixels::MAX).child(self.editor.clone()))
            .child(
                resizable_panel()
                    .size(px(440.))
                    .size_range(px(260.)..Pixels::MAX)
                    .visible(c_vis)
                    .child(div().size_full().child(self.claude.clone())),
            );
        let body = v_resizable("main-v")
            .child(resizable_panel().size_range(px(120.)..Pixels::MAX).child(main_row))
            .child(
                resizable_panel()
                    .size(px(230.))
                    .size_range(px(80.)..Pixels::MAX)
                    .visible(t_vis)
                    .child(self.terminal.clone()),
            );

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(bg)
            .text_color(fg)
            .key_context("Workspace")
            .on_action(cx.listener(|ws, _: &ToggleSkills, w, cx| ws.toggle("skills", w, cx)))
            .on_action(cx.listener(|ws, _: &ToggleFiles, w, cx| ws.toggle("files", w, cx)))
            .on_action(cx.listener(|ws, _: &ToggleClaude, w, cx| ws.toggle("claude", w, cx)))
            .on_action(cx.listener(|ws, _: &ToggleTerminal, w, cx| ws.toggle("terminal", w, cx)))
            .on_action(cx.listener(|ws, _: &FocusEditor, w, cx| ws.set_focus_mode(FocusMode::Editor, w, cx)))
            .on_action(cx.listener(|ws, _: &FocusClaude, w, cx| ws.set_focus_mode(FocusMode::Claude, w, cx)))
            .on_action(cx.listener(|ws, _: &Save, w, cx| ws.editor.update(cx, |e, cx| e.save_active(w, cx))))
            .on_action(cx.listener(|ws, _: &CloseTab, _, cx| ws.editor.update(cx, |e, cx| e.close_active(cx))))
            .on_action(cx.listener(|ws, _: &TogglePreview, w, cx| ws.editor.update(cx, |e, cx| e.toggle_preview(w, cx))))
            .on_action(cx.listener(|ws, _: &RenameTab, w, cx| ws.rename_tab(w, cx)))
            .on_action(cx.listener(|ws, _: &AskClaude, w, cx| {
                ws.show_claude = true;
                ws.claude.update(cx, |c, cx| c.focus_input(w, cx));
                cx.notify();
            }))
            .on_action(cx.listener(|ws, _: &NewTerminal, w, cx| {
                ws.show_terminal = true;
                ws.terminal.update(cx, |t, cx| {
                    t.new_tab(cx);
                    t.focus(w, cx);
                });
                cx.notify();
            }))
            .child(top)
            .child(div().flex_1().min_h_0().child(body))
    }
}
