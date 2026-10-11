//! Claude sessions of the window: the live ones (each its own `claude` process and chat
//! panel) and the folder's earlier ones from `~/.claude/projects`. This entity is both the
//! model and the sidebar list (sessions, their subagents as a small tree, and "Earlier"), so it
//! can move into a dock panel as it is.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use workbench_core::conversation::AgentStatus;
use workbench_core::history::{self, SessionInfo};
use workbench_core::host::Host;
use workbench_core::links::FileIndex;
use workbench_core::tabs::NameStore;

use crate::claude_panel::{ClaudePanel, ClaudePanelEvent};
use crate::claude_session::{ClaudeSession, SessionEvent};

pub enum SessionsEvent {
    OpenFile(PathBuf, Option<u32>),
    FileChanged(PathBuf),
    TurnEnded,
    /// The shown session changed (or one was added or closed).
    Switched,
}

pub struct SessionTab {
    pub number: usize,
    pub session: Entity<ClaudeSession>,
    pub panel: Entity<ClaudePanel>,
    _subs: Vec<Subscription>,
}

/// An earlier session of the folder, and the `claude` process writing it elsewhere, if any.
#[derive(Clone, Debug)]
pub struct Earlier {
    pub info: SessionInfo,
    pub running_pid: Option<u32>,
}

struct Rename {
    key: String,
    auto: String,
    input: Entity<InputState>,
    _sub: Subscription,
}

pub struct Sessions {
    host: Arc<dyn Host>,
    root: PathBuf,
    names: Rc<RefCell<NameStore>>,
    pub tabs: Vec<SessionTab>,
    pub active: usize,
    next: usize,
    pub earlier: Vec<Earlier>,
    /// The earlier session whose "running elsewhere" choices are shown.
    choosing: Option<String>,
    show_earlier: bool,
    renaming: Option<Rename>,
    index: Arc<FileIndex>,
    _scan: Option<Task<()>>,
}

impl EventEmitter<SessionsEvent> for Sessions {}

impl Sessions {
    pub fn new(
        host: Arc<dyn Host>,
        root: PathBuf,
        names: Rc<RefCell<NameStore>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            host,
            root,
            names,
            tabs: Vec::new(),
            active: 0,
            next: 1,
            earlier: Vec::new(),
            choosing: None,
            show_earlier: true,
            renaming: None,
            index: Arc::new(FileIndex::default()),
            _scan: None,
        };
        this.start_scanning(cx);
        this
    }

    pub fn set_index(&mut self, index: Arc<FileIndex>, cx: &mut Context<Self>) {
        self.index = index.clone();
        for t in &self.tabs {
            t.panel.update(cx, |p, cx| {
                p.index = index.clone();
                cx.notify();
            });
        }
    }

    pub fn active_tab(&self) -> Option<&SessionTab> {
        self.tabs.get(self.active)
    }

    // ----- live sessions -----

    fn add(&mut self, session: ClaudeSession, window: &mut Window, cx: &mut Context<Self>) {
        let session = cx.new(|_| session);
        let panel = cx.new(|cx| ClaudePanel::new(session.clone(), window, cx));
        let index = self.index.clone();
        panel.update(cx, |p, _| p.index = index);
        let subs = vec![
            cx.subscribe_in(
                &panel,
                window,
                |this, _, ev: &ClaudePanelEvent, window, cx| match ev {
                    ClaudePanelEvent::OpenFile(path, line) => {
                        cx.emit(SessionsEvent::OpenFile(path.clone(), *line))
                    }
                    ClaudePanelEvent::NewSession => this.new_session(window, cx),
                },
            ),
            cx.subscribe(&session, |_, _, ev: &SessionEvent, cx| match ev {
                SessionEvent::FileChanged(p) => cx.emit(SessionsEvent::FileChanged(p.clone())),
                SessionEvent::TurnEnded => cx.emit(SessionsEvent::TurnEnded),
            }),
            // Sidebar dots and the agent tree follow the session.
            cx.observe(&session, |_, _, cx| cx.notify()),
        ];
        let number = self.next;
        self.next += 1;
        self.tabs.push(SessionTab {
            number,
            session,
            panel,
            _subs: subs,
        });
        self.switch(self.tabs.len() - 1, window, cx);
    }

    pub fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let s = ClaudeSession::new(self.host.clone(), self.root.clone());
        self.add(s, window, cx);
    }

    pub fn switch(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active = ix;
        let t = &self.tabs[ix];
        t.session.update(cx, |s, _| s.unseen = false);
        t.panel.update(cx, |p, cx| p.focus_input(window, cx));
        cx.emit(SessionsEvent::Switched);
        cx.notify();
    }

    pub fn close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(ix);
        tab.session.update(cx, |s, _| s.stop());
        if self.tabs.is_empty() {
            self.new_session(window, cx);
            return;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.switch(self.active, window, cx);
        self.scan(cx);
    }

    fn key(&self, number: usize) -> String {
        NameStore::key(&self.root, &format!("session:{number}"))
    }

    fn auto_name(&self, tab: &SessionTab, cx: &App) -> String {
        tab.session
            .read(cx)
            .chat
            .title(40)
            .unwrap_or_else(|| format!("Session {}", tab.number))
    }

    pub fn name(&self, tab: &SessionTab, cx: &App) -> String {
        self.names
            .borrow()
            .get(&self.key(tab.number))
            .map(str::to_string)
            .unwrap_or_else(|| self.auto_name(tab, cx))
    }

    pub fn rename_active(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(t) = self.active_tab() {
            let key = self.key(t.number);
            let _ = self.names.borrow_mut().set(&key, name);
            cx.notify();
        }
    }

    // ----- earlier sessions -----

    /// Re-list earlier sessions and `claude` processes in the folder, now and every 10 s.
    fn start_scanning(&mut self, cx: &mut Context<Self>) {
        self._scan = Some(
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                loop {
                    if this.update(cx, |s, cx| s.scan(cx)).is_err() {
                        break;
                    }
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(10))
                        .await;
                }
            }),
        );
    }

    pub fn scan(&mut self, cx: &mut Context<Self>) {
        let host = self.host.clone();
        let root = self.root.clone();
        let open: Vec<String> = self
            .tabs
            .iter()
            .filter_map(|t| t.session.read(cx).chat.session_id.clone())
            .collect();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let found = cx
                .background_spawn(async move {
                    let Some(home) = workbench_core::skills::claude_home(host.as_ref()) else {
                        return Vec::new();
                    };
                    let sessions = history::list_sessions(host.as_ref(), &home, &root, 20);
                    // Processes can only be seen on this machine.
                    let procs = if host.is_remote() {
                        Vec::new()
                    } else {
                        history::claude_processes_in(&root, std::process::id())
                    };
                    let matched = history::match_processes(&procs, &sessions);
                    sessions
                        .into_iter()
                        .map(|info| Earlier {
                            running_pid: matched
                                .iter()
                                .find(|(_, id)| *id == info.id)
                                .map(|(pid, _)| *pid),
                            info,
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |s, cx| {
                s.earlier = found
                    .into_iter()
                    .filter(|e| !open.contains(&e.info.id))
                    .collect();
                cx.notify();
            });
        })
        .detach();
    }

    /// Resume an earlier session here: `fork` continues a copy; `wait_for` waits for that
    /// process elsewhere to exit first.
    pub fn resume(
        &mut self,
        id: &str,
        fork: bool,
        wait_for: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(e) = self.earlier.iter().find(|e| e.info.id == id).cloned() else {
            return;
        };
        if e.running_pid.is_some() && !fork && wait_for.is_none() {
            // Two processes must never write one session: ask first.
            self.choosing = Some(id.to_string());
            cx.notify();
            return;
        }
        self.choosing = None;
        let host = self.host.clone();
        let root = self.root.clone();
        let path = e.info.path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let cwd = root.to_string_lossy().to_string();
            let chat = cx
                .background_spawn(async move {
                    history::load_session(host.as_ref(), &path, &cwd)
                })
                .await;
            let _ = this.update_in(cx, |s, window, cx| match chat {
                Ok(chat) => {
                    let mut session = ClaudeSession::resumed(s.host.clone(), s.root.clone(), chat, fork);
                    if let Some(pid) = wait_for {
                        session.waiting_for = Some(pid);
                        session.chat.push_notice(format!(
                            "Waiting for the other terminal (process {pid}) to finish. Quit it there (type /exit or press Ctrl+D twice); this session resumes here by itself."
                        ));
                    }
                    s.add(session, window, cx);
                    if let Some(pid) = wait_for {
                        s.watch_takeover(pid, cx);
                    }
                    s.earlier.retain(|x| x.info.id != e.info.id);
                }
                Err(err) => eprintln!("workbench: could not read the session: {err}"),
            });
        })
        .detach();
    }

    /// Poll (with `ps`, no signals) until the other process exits, then let the session run.
    fn watch_takeover(&mut self, pid: u32, cx: &mut Context<Self>) {
        let Some(tab) = self
            .tabs
            .iter()
            .find(|t| t.session.read(cx).waiting_for == Some(pid))
        else {
            return;
        };
        let session = tab.session.downgrade();
        let host = self.host.clone();
        let root = self.root.clone();
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(2))
                    .await;
                let alive = cx
                    .background_spawn(async move { history::process_alive(pid) })
                    .await;
                if alive {
                    if session.upgrade().is_none() {
                        break;
                    }
                    continue;
                }
                // Reload the history the other terminal added, then continue here.
                let id = session
                    .update(cx, |s, _| s.chat.session_id.clone())
                    .ok()
                    .flatten();
                let (h, r) = (host.clone(), root.clone());
                let reloaded = cx
                    .background_spawn(async move {
                        let home = workbench_core::skills::claude_home(h.as_ref())?;
                        let id = id?;
                        let path =
                            history::project_sessions_dir(&home, &r).join(format!("{id}.jsonl"));
                        history::load_session(h.as_ref(), &path, &r.to_string_lossy()).ok()
                    })
                    .await;
                let _ = session.update(cx, |s, cx| {
                    if let Some(chat) = reloaded {
                        s.chat = chat;
                    }
                    s.waiting_for = None;
                    s.chat
                        .push_notice("The other terminal finished: this session continues here.");
                    cx.notify();
                });
                break;
            }
        })
        .detach();
    }

    // ----- sidebar list -----

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
        let sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::PressEnter { .. } | InputEvent::Blur => {
                if let Some(r) = this.renaming.take() {
                    let text = r.input.read(cx).value().trim().to_string();
                    let value = if text == r.auto { "" } else { text.as_str() };
                    let _ = this.names.borrow_mut().set(&r.key, value);
                    cx.notify();
                }
            }
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

    fn render_session_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, sel_bg, hover, blue, amber) = (
            theme.foreground,
            theme.muted_foreground,
            theme.accent,
            theme.secondary_hover,
            theme.blue,
            theme.warning,
        );
        let tab = &self.tabs[ix];
        let s = tab.session.read(cx);
        let (dot, tip) = if s.chat.needs_input() {
            (Some(amber), "needs permission")
        } else if s.waiting_for.is_some() {
            (Some(muted), "waiting for another terminal")
        } else if s.chat.busy {
            (Some(blue), "working")
        } else if s.unseen && ix != self.active {
            (Some(fg), "new activity")
        } else {
            (None, "")
        };
        let key = self.key(tab.number);
        let name = self.name(tab, cx);
        let auto = self.auto_name(tab, cx);
        let renamed = self.names.borrow().get(&key).is_some();
        let row = div()
            .id(("session-row", tab.number))
            .h(px(22.))
            .pl(px(14.))
            .pr_1()
            .flex()
            .items_center()
            .gap_1()
            .text_xs()
            .text_color(fg)
            .when(ix == self.active, |d| d.bg(sel_bg))
            .hover(|d| d.bg(hover))
            .child(Icon::new(IconName::Bot).xsmall().text_color(muted));
        if let Some(r) = self.renaming.as_ref().filter(|r| r.key == key) {
            return row
                .child(div().flex_1().child(Input::new(&r.input).xsmall()))
                .into_any_element();
        }
        let weak = cx.entity().downgrade();
        let (k1, a1, n1) = (key.clone(), auto.clone(), name.clone());
        row.child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .when(renamed, |d| d.italic())
                .child(name.clone()),
        )
        .children(dot.map(|c| {
            div()
                .size(px(7.))
                .flex_none()
                .rounded_full()
                .bg(c)
                .id(("dot", tab.number))
                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip).build(w, cx))
        }))
        .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
            if ev.click_count() >= 2 {
                this.start_rename(k1.clone(), a1.clone(), n1.clone(), window, cx);
            } else {
                this.switch(ix, window, cx);
            }
        }))
        .context_menu(move |menu, _, _| {
            let (w1, w2, w3) = (weak.clone(), weak.clone(), weak.clone());
            let (k, a, n, k2) = (key.clone(), auto.clone(), name.clone(), key.clone());
            menu.item(PopupMenuItem::new("Rename").on_click(move |_, window, cx| {
                let _ = w1.update(cx, |s, cx| {
                    s.start_rename(k.clone(), a.clone(), n.clone(), window, cx)
                });
            }))
            .item(PopupMenuItem::new("Reset name").on_click(move |_, _, cx| {
                let _ = w2.update(cx, |s, cx| {
                    let _ = s.names.borrow_mut().reset(&k2);
                    cx.notify();
                });
            }))
            .separator()
            .item(
                PopupMenuItem::new("Close session").on_click(move |_, window, cx| {
                    let _ = w3.update(cx, |s, cx| s.close(ix, window, cx));
                }),
            )
        })
        .into_any_element()
    }

    /// The session's subagents (this turn's) as a small tree under its row.
    fn render_agent_rows(&self, ix: usize, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (muted, hover, green, red, violet) = (
            theme.muted_foreground,
            theme.secondary_hover,
            theme.green,
            theme.red,
            theme.magenta,
        );
        let tab = &self.tabs[ix];
        let agents = tab.session.read(cx).chat.turn_agents().to_vec();
        let panel = tab.panel.downgrade();
        agents
            .into_iter()
            .map(|a| {
                let icon = match a.status {
                    AgentStatus::Running => Icon::new(IconName::LoaderCircle)
                        .xsmall()
                        .text_color(violet),
                    AgentStatus::Done => Icon::new(IconName::Check).xsmall().text_color(green),
                    AgentStatus::Failed => Icon::new(IconName::CircleX).xsmall().text_color(red),
                };
                let elapsed = a
                    .elapsed()
                    .map(|d| format!("{}s", d.as_secs()))
                    .unwrap_or_default();
                let title = if a.description.is_empty() {
                    a.kind.clone()
                } else {
                    a.description.clone()
                };
                let id = a.tool_use_id.clone();
                let panel = panel.clone();
                div()
                    .id(SharedString::from(format!("agent-row-{}", a.tool_use_id)))
                    .h(px(20.))
                    .pl(px(30.))
                    .pr_1()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(px(11.))
                    .text_color(muted)
                    .hover(|d| d.bg(hover))
                    .child(div().text_color(muted.opacity(0.6)).child("└"))
                    .child(icon)
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .when_some(a.current_tool.clone(), |d, t| {
                        d.child(div().flex_none().child(t))
                    })
                    .child(div().flex_none().child(elapsed))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.switch(ix, window, cx);
                        let _ = panel.update(cx, |p, cx| p.expand_agent(&id, cx));
                    }))
                    .into_any_element()
            })
            .collect()
    }

    fn render_earlier(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (fg, muted, hover, amber) = (
            theme.foreground,
            theme.muted_foreground,
            theme.secondary_hover,
            theme.warning,
        );
        let mut out = Vec::new();
        let open = self.show_earlier;
        out.push(
            div()
                .id("earlier-head")
                .h(px(20.))
                .pl(px(6.))
                .flex()
                .items_center()
                .gap_1()
                .text_size(px(10.5))
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
                .child(format!("Earlier ({})", self.earlier.len()))
                .on_click(cx.listener(|s, _, _, cx| {
                    s.show_earlier = !s.show_earlier;
                    cx.notify();
                }))
                .into_any_element(),
        );
        if !open {
            return out;
        }
        for (i, e) in self.earlier.iter().enumerate() {
            let id = e.info.id.clone();
            let when = e
                .info
                .last_activity
                .as_deref()
                .map(short_time)
                .unwrap_or_default();
            let running = e.running_pid;
            out.push(
                div()
                    .id(("earlier", i))
                    .pl(px(14.))
                    .pr_1()
                    .py(px(2.))
                    .flex()
                    .flex_col()
                    .hover(|d| d.bg(hover))
                    .child(
                        div()
                            .text_xs()
                            .text_color(fg)
                            .truncate()
                            .child(e.info.title.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .text_size(px(10.5))
                            .text_color(muted)
                            .child(format!("{when} · {} messages", e.info.messages))
                            .when(running.is_some(), |d| {
                                d.child(
                                    div()
                                        .text_color(amber)
                                        .child("· running in another terminal"),
                                )
                            }),
                    )
                    .on_click(
                        cx.listener(move |s, _, window, cx| s.resume(&id, false, None, window, cx)),
                    )
                    .into_any_element(),
            );
            if let (Some(pid), true) = (
                running,
                self.choosing.as_deref() == Some(e.info.id.as_str()),
            ) {
                let (id1, id2) = (e.info.id.clone(), e.info.id.clone());
                out.push(
                    div()
                        .pl(px(14.))
                        .pr_1()
                        .py_1()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_size(px(10.5))
                        .text_color(muted)
                        .child(
                            "Open in another terminal. Two processes must not write one session.",
                        )
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .child(
                                    Button::new(("fork", i))
                                        .xsmall()
                                        .primary()
                                        .label("Fork here")
                                        .on_click(cx.listener(move |s, _, window, cx| {
                                            s.resume(&id1, true, None, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new(("takeover", i))
                                        .xsmall()
                                        .label("Take over when it stops")
                                        .on_click(cx.listener(move |s, _, window, cx| {
                                            s.resume(&id2, false, Some(pid), window, cx)
                                        })),
                                ),
                        )
                        .into_any_element(),
                );
            }
        }
        out
    }
}

/// `2026-10-11T22:40:00.617Z` as `10-11 22:40` (UTC).
fn short_time(iso: &str) -> String {
    let date = iso.get(5..10).unwrap_or_default();
    let time = iso.get(11..16).unwrap_or_default();
    format!("{date} {time}")
}

impl Render for Sessions {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div()
            .id("sessions-list")
            .flex()
            .flex_col()
            .w_full()
            .overflow_y_scroll();
        for ix in 0..self.tabs.len() {
            col = col.child(self.render_session_row(ix, cx));
            for row in self.render_agent_rows(ix, cx) {
                col = col.child(row);
            }
        }
        if !self.earlier.is_empty() {
            for row in self.render_earlier(cx) {
                col = col.child(row);
            }
        }
        col
    }
}

/// The row of session tabs at the top of the Claude column, with "+" for a new one.
pub struct SessionTabs {
    sessions: Entity<Sessions>,
    _obs: Subscription,
}

impl SessionTabs {
    pub fn new(sessions: Entity<Sessions>, cx: &mut Context<Self>) -> Self {
        let obs = cx.observe(&sessions, |_, _, cx| cx.notify());
        Self {
            sessions,
            _obs: obs,
        }
    }
}

impl Render for SessionTabs {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, muted, border, active_bg, bar, hover, blue, amber) = (
            theme.foreground,
            theme.muted_foreground,
            theme.border,
            theme.tab_active,
            theme.tab_bar,
            theme.secondary_hover,
            theme.blue,
            theme.warning,
        );
        let s = self.sessions.read(cx);
        let mut row = div()
            .id("session-tabs")
            .h(px(26.))
            .flex_none()
            .flex()
            .items_center()
            .bg(bar)
            .border_b_1()
            .border_color(border)
            .overflow_x_scroll();
        for (ix, tab) in s.tabs.iter().enumerate() {
            let active = ix == s.active;
            let session = tab.session.read(cx);
            let dot = if session.chat.needs_input() {
                Some(amber)
            } else if session.chat.busy {
                Some(blue)
            } else if session.unseen && !active {
                Some(fg)
            } else {
                None
            };
            let sessions = self.sessions.downgrade();
            row = row.child(
                div()
                    .id(("session-tab", tab.number))
                    .h_full()
                    .px_2()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .max_w(px(220.))
                    .border_r_1()
                    .border_color(border)
                    .text_color(if active { fg } else { muted })
                    .when(active, |d| d.bg(active_bg).border_t_2().border_color(blue))
                    .hover(|d| d.bg(hover))
                    .child(Icon::new(IconName::Bot).xsmall())
                    .child(div().truncate().child(s.name(tab, cx)))
                    .children(dot.map(|c| div().size(px(6.)).flex_none().rounded_full().bg(c)))
                    .on_click(move |_, window, cx| {
                        let _ = sessions.update(cx, |s, cx| s.switch(ix, window, cx));
                    }),
            );
        }
        let sessions = self.sessions.downgrade();
        row.child(
            div()
                .id("session-new")
                .h_full()
                .px_2()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .text_xs()
                .text_color(muted)
                .hover(|d| d.bg(hover).text_color(fg))
                .child(Icon::new(IconName::Plus).xsmall())
                .child("New session")
                .tooltip(|w, cx| {
                    gpui_kit::component::tooltip::Tooltip::new("New Claude session (Cmd+N)")
                        .build(w, cx)
                })
                .on_click(move |_, window, cx| {
                    let _ = sessions.update(cx, |s, cx| s.new_session(window, cx));
                }),
        )
    }
}
