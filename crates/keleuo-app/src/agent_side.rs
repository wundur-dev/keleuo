//! The sidebar's Agent panel for one session: the conversation's subagents, live, as a small
//! list (shown when the backend reports subagents). Earlier and running sessions of the folder
//! are found in the session browser, not here: a session is one conversation.

use std::path::PathBuf;
use crate::panels::{LIST_SMALL, LIST_TEXT, ROW_HEIGHT};
use std::sync::Arc;

use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleuo_core::conversation::{AgentStatus, SubAgent};
use keleuo_core::host::{self, Host};

use crate::chat_panel::{ChatPanel, ChatPanelEvent};
use crate::agent_session::AgentSession;

pub struct AgentSide {
    session: Entity<AgentSession>,
    panel: Entity<ChatPanel>,
    focus: FocusHandle,
    /// The finished subagents are listed (they start folded away).
    finished_open: bool,
    _obs: Subscription,
}

impl AgentSide {
    pub fn new(session: Entity<AgentSession>, panel: Entity<ChatPanel>, cx: &mut Context<Self>) -> Self {
        let obs = cx.observe(&session, |_, _, cx| cx.notify());
        Self { session, panel, focus: cx.focus_handle(), finished_open: false, _obs: obs }
    }

    fn render_agents(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (muted, violet) = (theme.muted_foreground, theme.magenta);
        // Running subagents of any turn (one sent to the background outlives its turn), then
        // the finished ones, newest first, folded away until asked for.
        let agents: Vec<SubAgent> = self.session.read(cx).chat.agents.iter().rev().cloned().collect();
        let (running, finished): (Vec<_>, Vec<_>) = agents.into_iter().partition(SubAgent::running);
        let mut out = vec![
            crate::panels::section_header(
                "subagents-head".into(),
                Some(Icon::new(IconName::Bot).xsmall().text_color(violet.opacity(0.85))),
                "Subagents",
                Some(running.len().to_string()),
                None,
                true,
                cx,
            )
            .into_any_element(),
        ];
        // Nothing running: the header's 0 says so; a line only when there has been nothing yet.
        if running.is_empty() && finished.is_empty() {
            out.push(
                div()
                    .pl(px(24.))
                    .h(px(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .text_size(px(LIST_SMALL))
                    .text_color(muted.opacity(0.7))
                    .child("None yet")
                    .into_any_element(),
            );
        }
        for a in &running {
            out.push(self.render_row(a, cx));
        }
        if !finished.is_empty() {
            let open = self.finished_open;
            out.push(
                crate::panels::section_header(
                    "subagents-finished".into(),
                    None,
                    "Completed",
                    Some(finished.len().to_string()),
                    Some(open),
                    false,
                    cx,
                )
                .cursor_pointer()
                .on_click(cx.listener(|s, _, _, cx| {
                    s.finished_open = !s.finished_open;
                    cx.notify();
                }))
                .into_any_element(),
            );
            if open {
                for a in &finished {
                    out.push(self.render_row(a, cx));
                }
            }
        }
        out
    }

    /// One subagent: its status, what it was asked, the tool it runs and its time. A click
    /// watches it in its own panel.
    fn render_row(&self, a: &SubAgent, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, hover, green, red, violet) = (
            theme.sidebar_foreground,
            theme.muted_foreground,
            theme.secondary_hover,
            theme.green,
            theme.red,
            theme.magenta,
        );
        let icon = match a.status {
            AgentStatus::Running => Icon::new(IconName::LoaderCircle).xsmall().text_color(violet),
            AgentStatus::Done => Icon::new(IconName::Check).xsmall().text_color(green),
            AgentStatus::Failed => Icon::new(IconName::CircleX).xsmall().text_color(red),
        };
        let elapsed = a.elapsed().map(|d| format!("{}s", d.as_secs())).unwrap_or_default();
        let title = a.title();
        let id = a.tool_use_id.clone();
        let panel = self.panel.downgrade();
        div()
            .id(SharedString::from(format!("agent-row-{}", a.tool_use_id)))
            .h(px(ROW_HEIGHT))
            .pl(px(18.))
            .pr(px(6.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(LIST_TEXT))
            .text_color(if a.running() { fg } else { muted })
            .hover(|d| d.bg(hover))
            .child(icon)
            .child(div().flex_1().min_w_0().truncate().child(title))
            .when_some(a.current_tool.clone(), |d, t| d.child(div().flex_none().text_color(muted).child(t)))
            .child(div().flex_none().text_color(muted).child(elapsed))
            .on_click(move |_, _, cx| {
                // Watch it in its own docked panel; the workspace places that panel.
                let _ = panel.update(cx, |_, cx| cx.emit(ChatPanelEvent::OpenAgent(id.clone())));
            })
            .into_any_element()
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
        let bg = cx.theme().background;
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
