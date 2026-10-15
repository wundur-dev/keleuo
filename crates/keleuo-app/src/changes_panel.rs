//! The Changes panel: what the current turn, or the whole session, changed, file by file, with
//! line counts. Code first, then tests, then generated files (lock files, build output), folded.
//! A file opens as the diff of what the session changed in it.

use std::path::{Path, PathBuf};
use crate::panels::{LIST_SMALL, LIST_TEXT, LIST_TINY, ROW_HEIGHT};

use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleuo_core::changes::{FileChange, Group};

use crate::agent_session::AgentSession;
use crate::chat_panel::{ChatPanel, ChatPanelEvent};

pub struct ChangesPanel {
    session: Entity<AgentSession>,
    /// The chat, which the workspace listens to for opening diffs.
    panel: Entity<ChatPanel>,
    focus: FocusHandle,
    /// The whole session rather than the current turn.
    session_scope: bool,
    /// Generated files are listed (they start folded).
    generated_open: bool,
    _obs: Subscription,
}

impl ChangesPanel {
    pub fn new(session: Entity<AgentSession>, panel: Entity<ChatPanel>, cx: &mut Context<Self>) -> Self {
        let obs = cx.observe(&session, |_, _, cx| cx.notify());
        Self { session, panel, focus: cx.focus_handle(), session_scope: false, generated_open: false, _obs: obs }
    }

    /// Show the current turn's changes (`false`) or the session's.
    pub fn set_session_scope(&mut self, session: bool, cx: &mut Context<Self>) {
        self.session_scope = session;
        cx.notify();
    }

    fn scope_button(&self, label: &'static str, session: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let on = self.session_scope == session;
        div()
            .id(label)
            .px_2()
            .h(px(ROW_HEIGHT))
            .flex()
            .items_center()
            .rounded(px(5.))
            .text_size(px(LIST_SMALL))
            .cursor_pointer()
            .text_color(if on { theme.foreground } else { theme.muted_foreground })
            .when(on, |d| d.bg(theme.secondary))
            .when(!on, |d| d.hover(|d| d.bg(theme.secondary_hover)))
            .child(label)
            .on_click(cx.listener(move |p, _, _, cx| p.set_session_scope(session, cx)))
            .into_any_element()
    }

    fn row(&self, change: &FileChange, root: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, hover, green, red) =
            (theme.sidebar_foreground, theme.muted_foreground, theme.secondary_hover, theme.green, theme.red);
        let name = change.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let dir = change
            .path
            .parent()
            .and_then(|p| p.strip_prefix(root).ok())
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let path = change.path.clone();
        let panel = self.panel.downgrade();
        div()
            .id(SharedString::from(format!("change-{}", change.path.display())))
            .h(px(ROW_HEIGHT))
            .pl(px(10.))
            .pr(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(LIST_TEXT))
            .text_color(fg)
            .cursor_pointer()
            .hover(|d| d.bg(hover))
            .child(crate::icons::file(&name, cx))
            // The name gives way before the counts do.
            .child(div().flex_shrink(1.).min_w(px(40.)).truncate().child(name))
            .child(div().flex_1().min_w_0().truncate().text_size(px(LIST_SMALL)).text_color(muted).child(dir))
            .when(change.by_command, |d| d.child(div().flex_none().text_size(px(LIST_TINY)).text_color(muted).child("by a command")))
            .when(change.created, |d| d.child(div().flex_none().text_size(px(LIST_TINY)).text_color(green).child("new")))
            .child(div().flex_none().text_size(px(LIST_SMALL)).text_color(green).child(format!("+{}", change.added)))
            .child(div().flex_none().text_size(px(LIST_SMALL)).text_color(red).child(format!("−{}", change.removed)))
            .tooltip({
                let tip = match change.edits {
                    0 => "Changed by a command, found with git".to_string(),
                    1 => "1 edit".to_string(),
                    n => format!("{n} edits"),
                };
                move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)
            })
            .on_click(move |_, _, cx| {
                let _ = panel.update(cx, |_, cx| cx.emit(ChatPanelEvent::OpenDiff(path.clone())));
            })
            .into_any_element()
    }
}

impl Focusable for ChangesPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ChangesPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (changes, root): (Vec<FileChange>, PathBuf) = {
            let s = self.session.read(cx);
            let from = if self.session_scope { 0 } else { s.chat.turn_start() };
            (s.chat.changes(from), s.cwd.clone())
        };
        let theme = cx.theme();
        let (muted, bg, green, red, border) = (theme.muted_foreground, crate::theme::tool_ground(cx), theme.green, theme.red, theme.border);
        let (added, removed) = changes.iter().fold((0, 0), |(a, r), c| (a + c.added, r + c.removed));
        let mut col = div()
            .id("changes-panel")
            .size_full()
            .flex()
            .flex_col()
            .bg(bg)
            .overflow_y_scroll()
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_1()
                    .border_b_1()
                    .border_color(border)
                    .child(self.scope_button("This turn", false, cx))
                    .child(self.scope_button("This session", true, cx))
                    .child(div().flex_1())
                    .when(!changes.is_empty(), |d| {
                        d.child(div().text_size(px(LIST_SMALL)).text_color(muted).child(match changes.len() {
                            1 => "1 file".to_string(),
                            n => format!("{n} files"),
                        }))
                        .child(div().text_size(px(LIST_SMALL)).text_color(green).child(format!("+{added}")))
                        .child(div().text_size(px(LIST_SMALL)).text_color(red).child(format!("−{removed}")))
                    }),
            );
        if changes.is_empty() {
            let what = if self.session_scope { "No files changed in this session yet." } else { "No files changed in this turn." };
            return col
                .child(div().px_3().py_2().text_size(px(LIST_SMALL)).text_color(muted).child(what))
                .into_any_element();
        }
        for (group, title) in [(Group::Code, None), (Group::Tests, Some("Tests")), (Group::Generated, Some("Generated"))] {
            let in_group: Vec<&FileChange> = changes.iter().filter(|c| c.group == group).collect();
            if in_group.is_empty() {
                continue;
            }
            let open = group != Group::Generated || self.generated_open;
            if let Some(title) = title {
                let mut header = crate::panels::section_header(
                    SharedString::from(format!("changes-{title}")),
                    Some(Icon::new(if group == Group::Tests { IconName::CircleCheck } else { IconName::Settings }).xsmall().text_color(muted)),
                    title,
                    Some(in_group.len().to_string()),
                    (group == Group::Generated).then_some(open),
                    false,
                    cx,
                );
                if group == Group::Generated {
                    header = header.cursor_pointer().on_click(cx.listener(|p, _, _, cx| {
                        p.generated_open = !p.generated_open;
                        cx.notify();
                    }));
                }
                col = col.child(header);
            }
            if open {
                for c in in_group {
                    col = col.child(self.row(c, &root, cx));
                }
            }
        }
        col.into_any_element()
    }
}
