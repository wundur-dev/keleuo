//! The window: session tabs at the very top (in the title bar), and the shown session below.
//! Each session is a whole workspace (folder, one Claude conversation, terminals, files,
//! skills, dock layout). Switching a tab switches everything; hidden sessions keep running.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, TitleBar, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::*;
use workbench_core::host::{Host, LocalHost};

use crate::AppSettings;
use crate::actions::*;
use crate::claude_session::ClaudeSession;
use crate::panels::{self, start_tab_rename};
use crate::workspace::{Workspace, WorkspaceEvent};

struct Tab {
    number: usize,
    ws: Entity<Workspace>,
}

/// The "new session" chooser: which folder the new session works in.
struct Chooser {
    /// The folder of the session that asked (offered first, and for a worktree).
    current: Option<(Arc<dyn Host>, PathBuf)>,
    recent: Vec<PathBuf>,
    is_git: bool,
}

pub struct Shell {
    tabs: Vec<Tab>,
    active: usize,
    next: usize,
    chooser: Option<Chooser>,
    focus: FocusHandle,
    subs: HashMap<EntityId, Vec<Subscription>>,
}

impl Shell {
    pub fn new(
        host: Arc<dyn Host>,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            tabs: Vec::new(),
            active: 0,
            next: 1,
            chooser: None,
            focus: cx.focus_handle(),
            subs: HashMap::new(),
        };
        let session = ClaudeSession::new(host.clone(), root.clone());
        this.add_session(host, root, session, None, None, window, cx);
        this
    }

    pub fn active_workspace(&self) -> Option<Entity<Workspace>> {
        self.tabs.get(self.active).map(|t| t.ws.clone())
    }

    /// Open a session tab for `root` with this conversation.
    #[allow(clippy::too_many_arguments)]
    pub fn add_session(
        &mut self,
        host: Arc<dyn Host>,
        root: PathBuf,
        session: ClaudeSession,
        worktree: Option<String>,
        wait_for: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !host.is_remote() {
            let settings = cx.global::<AppSettings>().0.clone();
            settings.borrow_mut().add_recent(&root);
            let _ = settings.borrow().save();
        }
        let ws = cx.new(|cx| Workspace::new(host.clone(), root.clone(), session, window, cx));
        ws.update(cx, |w, _| w.worktree = worktree);
        if let Some(pid) = wait_for {
            let session = ws.read(cx).session.clone();
            crate::claude_side::watch_takeover(session, host, root, pid, cx);
        }
        let subs = vec![
            cx.subscribe_in(&ws, window, |shell, ws, ev: &WorkspaceEvent, window, cx| match ev {
                WorkspaceEvent::NewSession => shell.open_chooser(cx),
                WorkspaceEvent::Resume { chat, fork, wait_for } => {
                    let (host, root) = {
                        let w = ws.read(cx);
                        (w.host(), w.root().to_path_buf())
                    };
                    let mut session = ClaudeSession::resumed(host.clone(), root.clone(), (**chat).clone(), *fork);
                    if let Some(pid) = wait_for {
                        session.waiting_for = Some(*pid);
                        session.chat.push_notice(format!(
                            "Waiting for the other terminal (process {pid}) to finish. Quit it there (type /exit or press Ctrl+D twice); this session resumes here by itself."
                        ));
                    }
                    shell.add_session(host, root, session, None, *wait_for, window, cx);
                }
                WorkspaceEvent::ClaudeEdited(path) => shell.edited(ws.clone(), path.clone(), window, cx),
            }),
            // The tab's name and dot follow the session.
            cx.observe(&ws, |_, _, cx| cx.notify()),
        ];
        let number = self.next;
        self.next += 1;
        self.subs.insert(ws.entity_id(), subs);
        self.tabs.push(Tab { number, ws });
        self.activate(self.tabs.len() - 1, window, cx);
    }

    pub fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active = ix;
        self.chooser = None;
        let ws = self.tabs[ix].ws.clone();
        ws.update(cx, |w, cx| {
            w.session.update(cx, |s, _| s.unseen = false);
            w.focus_main(window, cx);
        });
        let title = format!("{} - Workbench", self.name(ix, cx));
        window.set_window_title(&title);
        cx.notify();
    }

    pub fn close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(ix);
        tab.ws
            .update(cx, |w, cx| w.session.update(cx, |s, _| s.stop()));
        self.subs.remove(&tab.ws.entity_id());
        if self.tabs.is_empty() {
            window.remove_window();
            return;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.activate(self.active, window, cx);
    }

    fn key(&self, ix: usize) -> String {
        panels::session_key(self.tabs[ix].number)
    }

    fn auto_name(&self, ix: usize, cx: &App) -> String {
        self.tabs[ix].ws.read(cx).auto_name(cx)
    }

    fn name(&self, ix: usize, cx: &App) -> String {
        panels::tab_label(&self.key(ix), self.auto_name(ix, cx), cx)
    }

    /// Claude in one session edited a file: warn the other sessions in the same folder that
    /// have it open (sessions in their own git worktree are separate folders and stay quiet).
    fn edited(
        &mut self,
        by: Entity<Workspace>,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(by_ix) = self.tabs.iter().position(|t| t.ws == by) else {
            return;
        };
        let by_name = self.name(by_ix, cx);
        let root = by.read(cx).root().to_path_buf();
        for t in &self.tabs {
            if t.ws == by {
                continue;
            }
            let same = t.ws.read(cx).root() == root.as_path() && t.ws.read(cx).has_open(&path, cx);
            if same {
                let name = by_name.clone();
                t.ws.update(cx, |w, cx| {
                    w.warn_edited_elsewhere(&path, &name, window, cx)
                });
            }
        }
    }

    // ----- new session -----

    pub fn open_chooser(&mut self, cx: &mut Context<Self>) {
        let current = self.active_workspace().map(|w| {
            let w = w.read(cx);
            (w.host(), w.root().to_path_buf())
        });
        let is_git = current
            .as_ref()
            .is_some_and(|(h, r)| !h.is_remote() && r.join(".git").exists());
        let mut recent = cx.global::<AppSettings>().0.borrow().existing_recent();
        if let Some((_, r)) = &current {
            recent.retain(|p| p != r);
        }
        self.chooser = Some(Chooser {
            current,
            recent,
            is_git,
        });
        cx.notify();
    }

    pub fn new_session_in(
        &mut self,
        host: Arc<dyn Host>,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = ClaudeSession::new(host.clone(), root.clone());
        self.add_session(host, root, session, None, None, window, cx);
    }

    /// A new session in a git worktree of the current folder: a new branch and folder next to
    /// the project (`git worktree add`), so parallel sessions do not edit the same files.
    pub fn new_session_in_worktree(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(ws) = self.active_workspace() else {
            return Err("no session".into());
        };
        let (host, root) = {
            let w = ws.read(cx);
            (w.host(), w.root().to_path_buf())
        };
        if host.is_remote() {
            return Err("worktrees over SSH are not supported yet".into());
        }
        let (path, branch) = make_worktree(&root)?;
        let session = ClaudeSession::new(Arc::new(LocalHost), path.clone());
        self.add_session(
            Arc::new(LocalHost),
            path,
            session,
            Some(branch),
            None,
            window,
            cx,
        );
        Ok(())
    }

    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update_in(cx, |s, window, cx| {
                    s.new_session_in(Arc::new(LocalHost), path, window, cx)
                });
            }
        })
        .detach();
    }

    // ----- control channel -----

    /// Commands for the window (`new`, `tab`, ...); others go to the shown session.
    pub fn command(
        &mut self,
        line: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let (cmd, arg) = line
            .trim()
            .split_once(' ')
            .map(|(a, b)| (a, b.trim()))
            .unwrap_or((line.trim(), ""));
        Some(match cmd {
            "new" => {
                let (host, root) = match self.active_workspace() {
                    Some(w) if arg.is_empty() => {
                        let w = w.read(cx);
                        (w.host(), w.root().to_path_buf())
                    }
                    _ => (Arc::new(LocalHost) as Arc<dyn Host>, PathBuf::from(arg)),
                };
                self.new_session_in(host, root, window, cx);
                "ok".into()
            }
            "new-worktree" => match self.new_session_in_worktree(window, cx) {
                Ok(()) => "ok".into(),
                Err(e) => e,
            },
            "chooser" => {
                self.open_chooser(cx);
                "ok".into()
            }
            "tab" => {
                let n = arg.parse::<usize>().unwrap_or(1);
                self.activate(n.saturating_sub(1), window, cx);
                "ok".into()
            }
            "close-tab" => {
                let ix = self.active;
                self.close(ix, window, cx);
                "ok".into()
            }
            "rename-tab" => {
                let key = self.key(self.active);
                let _ = cx.global::<panels::Names>().0.borrow_mut().set(&key, arg);
                cx.notify();
                "ok".into()
            }
            "tabs" => (0..self.tabs.len())
                .map(|i| {
                    format!(
                        "{}{}:{}",
                        if i == self.active { "*" } else { "" },
                        i + 1,
                        self.name(i, cx)
                    )
                })
                .collect::<Vec<_>>()
                .join(" | "),
            _ => return None,
        })
    }

    // ----- rendering -----

    fn render_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, hover, ring, blue, amber, bar) = (
            theme.foreground,
            theme.muted_foreground,
            theme.border,
            theme.secondary_hover,
            theme.ring,
            theme.blue,
            theme.warning,
            theme.title_bar,
        );
        let mut row = div()
            .id("session-tabs")
            .h_full()
            .flex()
            .items_end()
            .gap_px()
            .overflow_x_scroll()
            .min_w_0();
        for ix in 0..self.tabs.len() {
            let active = ix == self.active;
            let w = self.tabs[ix].ws.read(cx);
            let s = w.session.read(cx);
            let dot = if s.chat.needs_input() {
                Some(amber)
            } else if s.chat.busy || s.waiting_for.is_some() {
                Some(blue)
            } else if s.unseen && !active {
                Some(fg)
            } else {
                None
            };
            let worktree = w.worktree.clone();
            let key = self.key(ix);
            let auto = self.auto_name(ix, cx);
            let title = panels::title_element(&key, auto.clone(), dot, Some(IconName::Bot), cx);
            let weak = cx.entity().downgrade();
            let current = self.name(ix, cx);
            let (k1, a1, c1) = (key.clone(), auto.clone(), current.clone());
            let tab = div()
                .id(("session-tab", self.tabs[ix].number))
                .group("session-tab")
                .h(px(26.))
                .px_2()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .max_w(px(280.))
                .rounded_t_md()
                .text_xs()
                .text_color(if active { fg } else { muted })
                .when(active, |d| {
                    d.bg(theme.background).border_t_2().border_color(ring)
                })
                .when(!active, |d| d.bg(bar).hover(|d| d.bg(hover)))
                .child(div().min_w_0().truncate().child(title))
                .when_some(worktree, |d, b| {
                    d.child(
                        div()
                            .flex_none()
                            .px_1()
                            .rounded_sm()
                            .bg(border)
                            .text_size(px(10.))
                            .text_color(muted)
                            .child(format!("worktree {b}")),
                    )
                })
                .child(
                    div()
                        .id(("session-close", self.tabs[ix].number))
                        .flex_none()
                        .rounded_sm()
                        .invisible()
                        .group_hover("session-tab", |d| d.visible())
                        .when(active, |d| d.visible())
                        .hover(|d| d.bg(border))
                        .child(Icon::new(IconName::Close).xsmall().text_color(muted))
                        .on_click(cx.listener(move |s, _, window, cx| {
                            cx.stop_propagation();
                            s.close(ix, window, cx);
                        })),
                )
                .on_click(cx.listener(move |s, ev: &ClickEvent, window, cx| {
                    // In the title bar: keep a double click from zooming the window.
                    cx.stop_propagation();
                    if ev.click_count() >= 2 {
                        start_tab_rename(k1.clone(), a1.clone(), c1.clone(), window, cx);
                    } else {
                        s.activate(ix, window, cx);
                    }
                }))
                .context_menu(move |menu, _, _| {
                    let (w1, w2) = (weak.clone(), weak.clone());
                    let (k, a, c, k2) = (key.clone(), auto.clone(), current.clone(), key.clone());
                    menu.item(PopupMenuItem::new("Rename").on_click(move |_, window, cx| {
                        start_tab_rename(k.clone(), a.clone(), c.clone(), window, cx)
                    }))
                    .item(
                        PopupMenuItem::new("Reset name")
                            .on_click(move |_, window, cx| panels::reset_tab_name(&k2, window, cx)),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new("Close session").on_click(move |_, window, cx| {
                            let _ = w1.update(cx, |s, cx| s.close(ix, window, cx));
                        }),
                    )
                    .item(PopupMenuItem::new("New session").on_click(move |_, _, cx| {
                        let _ = w2.update(cx, |s, cx| s.open_chooser(cx));
                    }))
                });
            row = row.child(tab);
        }
        row.child(
            div()
                .id("session-new")
                .h(px(26.))
                .px_2()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .text_xs()
                .text_color(muted)
                .rounded_t_md()
                .hover(|d| d.bg(hover).text_color(fg))
                .child(Icon::new(IconName::Plus).xsmall())
                .child("New session")
                .tooltip(|w, cx| {
                    gpui_kit::component::tooltip::Tooltip::new("New session (Cmd+N)").build(w, cx)
                })
                .on_click(cx.listener(|s, _, _, cx| {
                    cx.stop_propagation();
                    s.open_chooser(cx)
                })),
        )
        .into_any_element()
    }

    fn render_chooser(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = self.chooser.as_ref()?;
        let theme = cx.theme();
        let (fg, muted, border, hover, bg) = (
            theme.foreground,
            theme.muted_foreground,
            theme.border,
            theme.secondary_hover,
            theme.popover,
        );
        let row = |id: SharedString, icon: IconName, title: String, sub: String| {
            div()
                .id(id)
                .px_2()
                .py_1()
                .rounded_md()
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .hover(|d| d.bg(hover))
                .child(Icon::new(icon).small().text_color(muted))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(div().text_sm().text_color(fg).truncate().child(title))
                        .when(!sub.is_empty(), |d| {
                            d.child(div().text_xs().text_color(muted).truncate().child(sub))
                        }),
                )
        };
        let mut list = div().flex().flex_col().gap_px();
        if let Some((host, root)) = c.current.clone() {
            let name = folder_name(&root);
            list = list.child(
                row(
                    "ch-same".into(),
                    IconName::FolderOpen,
                    format!("{name}: same folder"),
                    root.display().to_string(),
                )
                .on_click(cx.listener(move |s, _, window, cx| {
                    s.new_session_in(host.clone(), root.clone(), window, cx)
                })),
            );
            if c.is_git {
                list = list.child(
                    row(
                        "ch-worktree".into(),
                        IconName::Copy,
                        format!("{name}: new session in a git worktree"),
                        "A new branch and folder next to the project, so the two sessions do not edit the same files".into(),
                    )
                    .on_click(cx.listener(|s, _, window, cx| {
                        if let Err(e) = s.new_session_in_worktree(window, cx) {
                            window.push_notification(format!("Could not make a worktree: {e}"), cx);
                        }
                    })),
                );
            }
        }
        for (i, p) in c.recent.iter().enumerate() {
            let path = p.clone();
            list = list.child(
                row(
                    SharedString::from(format!("ch-recent-{i}")),
                    IconName::Folder,
                    folder_name(p),
                    p.display().to_string(),
                )
                .on_click(cx.listener(move |s, _, window, cx| {
                    s.new_session_in(Arc::new(LocalHost), path.clone(), window, cx)
                })),
            );
        }
        list = list.child(
            row(
                "ch-browse".into(),
                IconName::Search,
                "Choose a folder...".into(),
                String::new(),
            )
            .on_click(cx.listener(|s, _, window, cx| s.browse(window, cx))),
        );
        Some(
            div()
                .absolute()
                .top(px(34.))
                .left(px(80.))
                .w(px(460.))
                .p_2()
                .flex()
                .flex_col()
                .gap_1()
                .rounded_lg()
                .border_1()
                .border_color(border)
                .bg(bg)
                .shadow_lg()
                .on_mouse_down_out(cx.listener(|s, _, _, cx| {
                    s.chooser = None;
                    cx.notify();
                }))
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(muted)
                        .child("NEW SESSION IN"),
                )
                .child(list)
                .into_any_element(),
        )
    }
}

fn folder_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| p.display().to_string())
}

/// `git worktree add -b workbench/<name>-<n> <project>-<n>` next to the project.
fn make_worktree(root: &Path) -> Result<(PathBuf, String), String> {
    let parent = root.parent().ok_or("the folder has no parent")?;
    let name = folder_name(root);
    let mut n = 2;
    let (path, branch) = loop {
        let path = parent.join(format!("{name}-{n}"));
        let branch = format!("workbench/{name}-{n}");
        if !path.exists() {
            break (path, branch);
        }
        n += 1;
    };
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["worktree", "add", "-b", &branch])
        .arg(&path)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok((path, branch))
}

impl Focusable for Shell {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, fg, hover, muted) = (
            theme.background,
            theme.foreground,
            theme.secondary_hover,
            theme.muted_foreground,
        );
        let button = |id: &'static str, icon: IconName, tip: &'static str| {
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
        };
        let tabs = self.render_tabs(cx);
        let to_ws = |f: fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
            cx.listener(
                move |s: &mut Shell,
                      _: &ClickEvent,
                      window: &mut Window,
                      cx: &mut Context<Shell>| {
                    if let Some(ws) = s.active_workspace() {
                        ws.update(cx, |w, cx| f(w, window, cx));
                    }
                },
            )
        };
        let top = TitleBar::new().h(px(34.)).pl(px(76.)).child(
            div()
                .size_full()
                .flex()
                .items_end()
                .gap_1()
                .child(div().flex_1().min_w_0().h_full().child(tabs))
                .child(
                    div()
                        .h_full()
                        .flex()
                        .items_center()
                        .gap_1()
                        .pr_1()
                        .child(
                            button("t-sidebar", IconName::PanelLeft, "Sidebar (Cmd+B)")
                                .on_click(to_ws(|w, win, cx| w.toggle_sidebar_pub(win, cx))),
                        )
                        .child(
                            button(
                                "t-split",
                                IconName::PanelRight,
                                "Split the shown file or terminal (Cmd+\\)",
                            )
                            .on_click(to_ws(|w, win, cx| w.split_right(win, cx))),
                        )
                        .child(
                            button("t-reset", IconName::RotateCw, "Reset layout")
                                .on_click(to_ws(|w, win, cx| w.reset_layout(win, cx))),
                        ),
                ),
        );
        let body = self.active_workspace().map(|w| w.into_any_element());
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(bg)
            .text_color(fg)
            .track_focus(&self.focus)
            .on_action(cx.listener(|s, _: &NewSession, _, cx| s.open_chooser(cx)))
            .on_action(cx.listener(|s, _: &CloseSession, w, cx| {
                let ix = s.active;
                s.close(ix, w, cx)
            }))
            .on_action(cx.listener(|s, _: &NextSession, w, cx| {
                let ix = (s.active + 1) % s.tabs.len().max(1);
                s.activate(ix, w, cx)
            }))
            .on_action(cx.listener(|s, _: &PrevSession, w, cx| {
                let n = s.tabs.len().max(1);
                let ix = (s.active + n - 1) % n;
                s.activate(ix, w, cx)
            }))
            .on_action(cx.listener(|s, _: &OpenFolder, w, cx| s.browse(w, cx)))
            .on_action(cx.listener(|s, _: &RenameSession, w, cx| {
                let ix = s.active;
                let (key, auto, current) = (s.key(ix), s.auto_name(ix, cx), s.name(ix, cx));
                start_tab_rename(key, auto, current, w, cx);
            }))
            .child(top)
            .child(div().flex_1().min_h_0().children(body))
            .children(self.render_chooser(cx))
    }
}
