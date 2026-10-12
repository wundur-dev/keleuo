//! The sidebar's Agent panel for one session: the conversation's subagents, live, as a small
//! list (shown when the backend reports subagents). Earlier and running sessions of the folder
//! are found in the session browser, not here: a session is one conversation.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleion_core::conversation::AgentStatus;
use keleion_core::host::{self, Host};

use crate::chat_panel::ChatPanel;
use crate::agent_session::AgentSession;

pub struct AgentSide {
    session: Entity<AgentSession>,
    panel: Entity<ChatPanel>,
    focus: FocusHandle,
    _obs: Subscription,
}

impl AgentSide {
    pub fn new(session: Entity<AgentSession>, panel: Entity<ChatPanel>, cx: &mut Context<Self>) -> Self {
        let obs = cx.observe(&session, |_, _, cx| cx.notify());
        Self { session, panel, focus: cx.focus_handle(), _obs: obs }
    }

    fn render_agents(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (fg, muted, hover, green, red, violet) = (
            theme.sidebar_foreground,
            theme.muted_foreground,
            theme.secondary_hover,
            theme.green,
            theme.red,
            theme.magenta,
        );
        let agents = self.session.read(cx).chat.turn_agents().to_vec();
        let panel = self.panel.downgrade();
        let mut out = vec![
            crate::panels::section_header(
                "subagents-head".into(),
                Some(Icon::new(IconName::Bot).xsmall().text_color(violet.opacity(0.85))),
                "Subagents",
                Some(agents.len().to_string()),
                None,
                true,
                cx,
            )
            .into_any_element(),
        ];
        if agents.is_empty() {
            out.push(
                div()
                    .pl(px(24.))
                    .h(px(20.))
                    .flex()
                    .items_center()
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
                    .pl(px(18.))
                    .pr(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(11.))
                    .text_color(fg)
                    .hover(|d| d.bg(hover))
                    .child(icon)
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .when_some(a.current_tool.clone(), |d, t| {
                        d.child(div().flex_none().text_color(muted).child(t))
                    })
                    .child(div().flex_none().text_color(muted).child(elapsed))
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
                s.chat
                    .push_notice("The other terminal finished: this session continues here.");
                s.takeover_done(cx);
            });
            break;
        }
    })
    .detach();
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
        col
    }
}
