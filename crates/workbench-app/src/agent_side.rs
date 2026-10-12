//! The sidebar's agent panel for one session: the conversation's subagents (live, as a small
//! tree) and the folder's earlier sessions of the same backend, which resume as new sessions.
//! A session running in another terminal offers "Fork here" or "Take over when it stops".
//! Each part shows only when the backend has it.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use workbench_core::conversation::{AgentStatus, Conversation};
use workbench_core::agent::SessionInfo;
use workbench_core::host::{self, Host};

use crate::chat_panel::ChatPanel;
use crate::agent_session::AgentSession;

/// An earlier session of the folder, and the agent process writing it elsewhere, if any.
#[derive(Clone, Debug)]
pub struct Earlier {
    pub info: SessionInfo,
    pub running_pid: Option<u32>,
}

pub enum AgentSideEvent {
    /// Open this earlier conversation as a new session tab.
    Resume {
        chat: Box<Conversation>,
        fork: bool,
        wait_for: Option<u32>,
    },
}

pub struct AgentSide {
    host: Arc<dyn Host>,
    root: PathBuf,
    session: Entity<AgentSession>,
    panel: Entity<ChatPanel>,
    pub earlier: Vec<Earlier>,
    /// The earlier session whose "running elsewhere" choices are shown.
    choosing: Option<String>,
    show_earlier: bool,
    focus: FocusHandle,
    _scan: Option<Task<()>>,
    _obs: Subscription,
}

impl EventEmitter<AgentSideEvent> for AgentSide {}

impl AgentSide {
    pub fn new(
        host: Arc<dyn Host>,
        root: PathBuf,
        session: Entity<AgentSession>,
        panel: Entity<ChatPanel>,
        cx: &mut Context<Self>,
    ) -> Self {
        let obs = cx.observe(&session, |_, _, cx| cx.notify());
        let mut this = Self {
            host,
            root,
            session,
            panel,
            earlier: Vec::new(),
            choosing: None,
            show_earlier: true,
            focus: cx.focus_handle(),
            _scan: None,
            _obs: obs,
        };
        this.start_scanning(cx);
        this
    }

    /// Re-list earlier sessions and agent processes in the folder, now and every 10 s.
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
        let own = self.session.read(cx).chat.session_id.clone();
        let backend = self.session.read(cx).backend.clone();
        if !backend.capabilities().earlier_sessions {
            return;
        }
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let found = cx
                .background_spawn(async move {
                    let sessions = backend.earlier_sessions(host.as_ref(), &root, 20);
                    let matched =
                        backend.running_elsewhere(host.as_ref(), &root, &sessions, std::process::id());
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
                    .filter(|e| Some(&e.info.id) != own.as_ref())
                    .collect();
                cx.notify();
            });
        })
        .detach();
    }

    /// Resume an earlier session as a new session tab: `fork` continues a copy; `wait_for`
    /// waits for that process elsewhere to exit first.
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
        let backend = self.session.read(cx).backend.clone();
        let id = e.info.id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let chat = cx
                .background_spawn(async move { backend.load_session(host.as_ref(), &root, &id) })
                .await;
            let _ = this.update(cx, |_, cx| match chat {
                Ok(chat) => cx.emit(AgentSideEvent::Resume {
                    chat: Box::new(chat),
                    fork,
                    wait_for,
                }),
                Err(err) => eprintln!("workbench: could not read the session: {err}"),
            });
        })
        .detach();
    }

    fn render_agents(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (muted, hover, green, red, violet) = (
            theme.muted_foreground,
            theme.secondary_hover,
            theme.green,
            theme.red,
            theme.magenta,
        );
        let agents = self.session.read(cx).chat.turn_agents().to_vec();
        let panel = self.panel.downgrade();
        let mut out = vec![
            div()
                .h(px(20.))
                .pl(px(6.))
                .flex()
                .items_center()
                .text_size(px(10.5))
                .text_color(muted)
                .child(format!("Subagents ({})", agents.len()))
                .into_any_element(),
        ];
        if agents.is_empty() {
            out.push(
                div()
                    .pl(px(14.))
                    .h(px(20.))
                    .text_size(px(11.))
                    .text_color(muted.opacity(0.7))
                    .child("None in this turn")
                    .into_any_element(),
            );
        }
        for a in agents {
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
            out.push(
                div()
                    .id(SharedString::from(format!("agent-row-{}", a.tool_use_id)))
                    .h(px(20.))
                    .pl(px(14.))
                    .pr_1()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(px(11.))
                    .text_color(muted)
                    .hover(|d| d.bg(hover))
                    .child(icon)
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .when_some(a.current_tool.clone(), |d, t| {
                        d.child(div().flex_none().child(t))
                    })
                    .child(div().flex_none().child(elapsed))
                    .on_click(move |_, window, cx| {
                        let _ = panel.update(cx, |p, cx| {
                            p.expand_agent(&id, cx);
                            p.focus_input(window, cx);
                        });
                    })
                    .into_any_element(),
            );
        }
        out
    }

    fn render_earlier(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (fg, muted, hover, amber) = (
            theme.foreground,
            theme.muted_foreground,
            theme.secondary_hover,
            theme.warning,
        );
        let open = self.show_earlier;
        let can_fork = self.session.read(cx).backend.capabilities().fork;
        let mut out = vec![
            div()
                .id("earlier-head")
                .h(px(20.))
                .mt_1()
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
                .child(format!("Earlier sessions here ({})", self.earlier.len()))
                .on_click(cx.listener(|s, _, _, cx| {
                    s.show_earlier = !s.show_earlier;
                    cx.notify();
                }))
                .into_any_element(),
        ];
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
                                .when(can_fork, |d| d.child(
                                    Button::new(("fork", i))
                                        .xsmall()
                                        .primary()
                                        .label("Fork here")
                                        .on_click(cx.listener(move |s, _, window, cx| {
                                            s.resume(&id1, true, None, window, cx)
                                        })),
                                ))
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

/// Poll (with `ps`, no signals) until the other process exits, then let the session run.
pub fn watch_takeover(
    session: Entity<AgentSession>,
    host: Arc<dyn Host>,
    root: PathBuf,
    pid: u32,
    cx: &mut App,
) {
    let session = session.downgrade();
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(2))
                .await;
            let alive = cx
                .background_spawn(async move { host::process_alive(pid) })
                .await;
            if alive {
                if session.upgrade().is_none() {
                    break;
                }
                continue;
            }
            // Reload the history the other terminal added, then continue here.
            let Ok((id, backend)) =
                session.update(cx, |s, _| (s.chat.session_id.clone(), s.backend.clone()))
            else {
                break;
            };
            let (h, r) = (host.clone(), root.clone());
            let reloaded = cx
                .background_spawn(async move { backend.load_session(h.as_ref(), &r, &id?).ok() })
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

/// `2026-10-11T22:40:00.617Z` as `10-11 22:40` (UTC).
fn short_time(iso: &str) -> String {
    let date = iso.get(5..10).unwrap_or_default();
    let time = iso.get(11..16).unwrap_or_default();
    format!("{date} {time}")
}

impl Focusable for AgentSide {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AgentSide {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bg = cx.theme().sidebar;
        let mut col = div()
            .id("agent-side")
            .flex()
            .flex_col()
            .size_full()
            .bg(bg)
            .overflow_y_scroll();
        let caps = self.session.read(cx).backend.capabilities();
        if caps.subagents {
            for row in self.render_agents(cx) {
                col = col.child(row);
            }
        }
        if caps.earlier_sessions {
            for row in self.render_earlier(cx) {
                col = col.child(row);
            }
        }
        col
    }
}
