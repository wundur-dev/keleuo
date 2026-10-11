//! The Claude panel: the chat rendered natively from the stream. Assistant text as markdown,
//! tool calls as collapsible cards, file edits as diffs, permission prompts with Allow and
//! Deny, and a one-line status in the header. Every file reference is a link.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use workbench_core::conversation::{Item, PermissionPrompt, ToolCall};
use workbench_core::diff::{FileDiff, LineKind};
use workbench_core::links::{self, FileIndex};
use workbench_core::summary::one_line;

use crate::claude_session::ClaudeSession;

pub enum ClaudePanelEvent {
    OpenFile(PathBuf, Option<u32>),
}

pub struct ClaudePanel {
    pub session: Entity<ClaudeSession>,
    input: Entity<TextareaState>,
    scroll: ScrollHandle,
    /// Tool cards the user opened (or closed, for diffs which start open).
    toggled: HashSet<String>,
    pub index: Arc<FileIndex>,
    follow: bool,
    last_revision: u64,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ClaudePanelEvent> for ClaudePanel {}

const MONO: &str = "Menlo";

impl ClaudePanel {
    pub fn new(
        session: Entity<ClaudeSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 10)
                .placeholder("Message Claude (Enter sends, Shift+Enter new line)")
                .submit_on_enter(true)
        });
        let s1 = cx.subscribe_in(
            &input,
            window,
            |panel, input, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = ev {
                    let text = input.read(cx).value().to_string();
                    if text.trim().is_empty() {
                        return;
                    }
                    input.update(cx, |s, cx| s.set_value("", window, cx));
                    panel.follow = true;
                    panel.session.update(cx, |s, cx| s.send(&text, cx));
                }
            },
        );
        let s2 = cx.observe(&session, |panel, session, cx| {
            let rev = session.read(cx).chat.revision;
            if rev != panel.last_revision {
                panel.last_revision = rev;
                if panel.follow {
                    panel.scroll.scroll_to_bottom();
                }
                cx.notify();
            }
        });
        Self {
            session,
            input,
            scroll: ScrollHandle::new(),
            toggled: HashSet::new(),
            index: Arc::new(FileIndex::default()),
            follow: true,
            last_revision: 0,
            _subs: vec![s1, s2],
        }
    }

    pub fn focus_input(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |s, cx| s.focus(window, cx));
    }

    fn open(&self, path: PathBuf, line: Option<u32>, cx: &mut Context<Self>) {
        cx.emit(ClaudePanelEvent::OpenFile(path, line));
    }

    fn markdown(
        &self,
        id: (&'static str, usize),
        text: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let md = self.index.linkify_markdown(text);
        let entity = cx.entity().downgrade();
        TextView::markdown(id, md)
            .selectable(true)
            .text_sm()
            .on_link_click(move |url, _, _, cx| {
                if let Some((path, line)) = links::parse_url(url) {
                    let _ = entity.update(cx, |p, cx| p.open(path, line, cx));
                } else {
                    cx.open_url(url);
                }
            })
            .into_any_element()
    }

    /// Plain text where file references are clickable spans.
    fn linked_text(
        &self,
        id: SharedString,
        text: &str,
        color: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let found = self.index.find_links(text);
        if found.is_empty() {
            return div().child(text.to_string()).into_any_element();
        }
        let link_color = cx.theme().link;
        let ranges: Vec<std::ops::Range<usize>> = found.iter().map(|l| l.start..l.end).collect();
        let targets: Vec<(PathBuf, Option<u32>)> =
            found.into_iter().map(|l| (l.path, l.line)).collect();
        let mut highlights = Vec::new();
        for r in &ranges {
            highlights.push((
                r.clone(),
                HighlightStyle {
                    color: Some(link_color),
                    underline: Some(UnderlineStyle {
                        thickness: px(1.),
                        color: Some(link_color),
                        wavy: false,
                    }),
                    ..Default::default()
                },
            ));
        }
        let style = TextStyle {
            color,
            font_family: MONO.into(),
            font_size: px(11.).into(),
            ..Default::default()
        };
        let styled = StyledText::new(text.to_string()).with_default_highlights(&style, highlights);
        let entity = cx.entity().downgrade();
        InteractiveText::new(id, styled)
            .on_click(ranges, move |ix, _, cx| {
                if let Some((path, line)) = targets.get(ix).cloned() {
                    let _ = entity.update(cx, |p, cx| p.open(path, line, cx));
                }
            })
            .into_any_element()
    }

    fn render_diff(&self, key: &str, diff: &FileDiff, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (green, red, muted, border) =
            (theme.green, theme.red, theme.muted_foreground, theme.border);
        let mut col = div()
            .flex()
            .flex_col()
            .font_family(MONO)
            .text_size(px(11.5))
            .line_height(px(16.));
        let max_lines = 400;
        let mut shown = 0;
        for hunk in &diff.hunks {
            if diff.hunks.len() > 1 || hunk.old_start > 0 {
                col = col.child(
                    div()
                        .px_1()
                        .text_color(muted)
                        .bg(border.opacity(0.4))
                        .child(format!("@@ -{} +{} @@", hunk.old_start, hunk.new_start)),
                );
            }
            let mut new_line = hunk.new_start.max(1);
            for line in &hunk.lines {
                shown += 1;
                if shown > max_lines {
                    break;
                }
                let (sign, bg, fg) = match line.kind {
                    LineKind::Added => ("+", green.opacity(0.16), green),
                    LineKind::Removed => ("-", red.opacity(0.16), red),
                    LineKind::Context => (" ", transparent_black(), muted),
                };
                let number = if line.kind == LineKind::Removed {
                    String::new()
                } else {
                    new_line.to_string()
                };
                if line.kind != LineKind::Removed {
                    new_line += 1;
                }
                let path = diff.path.clone();
                let target_line = number.parse::<u32>().ok();
                col = col.child(
                    div()
                        .id(SharedString::from(format!("{key}-{shown}")))
                        .flex()
                        .bg(bg)
                        .cursor_pointer()
                        .on_click(
                            cx.listener(move |p, _, _, cx| p.open(path.clone(), target_line, cx)),
                        )
                        .child(
                            div()
                                .w(px(32.))
                                .flex_none()
                                .text_right()
                                .pr_1()
                                .text_color(muted.opacity(0.7))
                                .child(number),
                        )
                        .child(div().w(px(12.)).flex_none().text_color(fg).child(sign))
                        .child(
                            div()
                                .flex_1()
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .child(line.text.clone()),
                        ),
                );
            }
        }
        if shown > max_lines {
            col = col.child(
                div()
                    .px_1()
                    .text_color(muted)
                    .child(format!("... {} more lines", shown - max_lines)),
            );
        }
        col.into_any_element()
    }

    fn render_tool(&self, ix: usize, tool: &ToolCall, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, card_bg, hover) = (
            theme.foreground,
            theme.muted_foreground,
            theme.border,
            theme.secondary,
            theme.secondary_hover,
        );
        let (green, red) = (theme.green, theme.red);
        // Diffs start open; other cards start closed.
        let default_open = tool.diff.is_some();
        let open = default_open != self.toggled.contains(&tool.id);
        let id = tool.id.clone();
        let status = if !tool.done() {
            Icon::new(IconName::LoaderCircle).xsmall().text_color(muted)
        } else if tool.is_error {
            Icon::new(IconName::CircleX).xsmall().text_color(red)
        } else {
            Icon::new(IconName::Check).xsmall().text_color(green)
        };
        let stats = tool.diff.as_ref().map(|d| {
            div()
                .flex()
                .gap_1()
                .flex_none()
                .child(div().text_color(green).child(format!("+{}", d.added())))
                .child(div().text_color(red).child(format!("-{}", d.removed())))
        });
        let summary_el = self.linked_text(
            SharedString::from(format!("sum-{}", tool.id)),
            &tool.summary,
            muted,
            cx,
        );
        let header = div()
            .id(SharedString::from(format!("tool-h-{}", tool.id)))
            .h(px(22.))
            .px_1()
            .flex()
            .items_center()
            .gap_1()
            .text_xs()
            .cursor_pointer()
            .hover(|d| d.bg(hover))
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall()
                .text_color(muted),
            )
            .child(status)
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(fg)
                    .child(tool.name.clone()),
            )
            .when(tool.parent.is_some(), |d| {
                d.child(div().text_color(muted).child("(subagent)"))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(MONO)
                    .child(summary_el),
            )
            .children(stats)
            .on_click(cx.listener(move |p, _, _, cx| {
                if !p.toggled.remove(&id) {
                    p.toggled.insert(id.clone());
                }
                cx.notify();
            }));
        let mut card = div()
            .flex()
            .flex_col()
            .border_1()
            .border_color(border)
            .rounded_md()
            .bg(card_bg.opacity(0.5))
            .overflow_hidden()
            .child(header);
        if open {
            let mut body = div().flex().flex_col().border_t_1().border_color(border);
            if let Some(diff) = &tool.diff {
                body = body.child(self.render_diff(&format!("d{ix}"), diff, cx));
            } else {
                let input = serde_json::to_string_pretty(&tool.input).unwrap_or_default();
                body = body.child(
                    div()
                        .px_2()
                        .py_1()
                        .font_family(MONO)
                        .text_size(px(11.))
                        .text_color(muted)
                        .max_h(px(160.))
                        .overflow_hidden()
                        .child(input),
                );
            }
            if let Some(result) = &tool.result
                && tool.diff.is_none()
            {
                let text: String = result.lines().take(60).collect::<Vec<_>>().join("\n");
                let more = result.lines().count().saturating_sub(60);
                body = body.child(
                    div()
                        .px_2()
                        .py_1()
                        .border_t_1()
                        .border_color(border)
                        .font_family(MONO)
                        .text_size(px(11.))
                        .text_color(if tool.is_error { red } else { fg })
                        .child(self.linked_text(
                            SharedString::from(format!("res-{}", tool.id)),
                            &text,
                            fg,
                            cx,
                        ))
                        .when(more > 0, |d| {
                            d.child(
                                div()
                                    .text_color(muted)
                                    .child(format!("... {more} more lines")),
                            )
                        }),
                );
            } else if let (Some(result), true) = (&tool.result, tool.is_error) {
                body = body.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(red)
                        .child(one_line(result, 300)),
                );
            }
            card = card.child(body);
        }
        card.into_any_element()
    }

    fn render_permission(&self, p: &PermissionPrompt, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (warning, muted, fg, border) = (
            theme.warning,
            theme.muted_foreground,
            theme.foreground,
            theme.border,
        );
        let mut card = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(if p.answer.is_none() { warning } else { border })
            .text_xs()
            .child(
                div()
                    .flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Icon::new(IconName::TriangleAlert)
                            .xsmall()
                            .text_color(warning),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(fg)
                            .child(format!("Allow {}?", p.tool_name)),
                    ),
            )
            .child(
                div()
                    .font_family(MONO)
                    .text_color(fg)
                    .child(p.summary.clone()),
            );
        if let Some(d) = &p.description
            && d != &p.summary
        {
            card = card.child(div().text_color(muted).child(d.clone()));
        }
        match p.answer {
            None => {
                let (rid1, rid2) = (p.request_id.clone(), p.request_id.clone());
                card =
                    card.child(
                        div()
                            .flex()
                            .gap_2()
                            .pt_1()
                            .child(
                                gpui_kit::component::button::Button::new(SharedString::from(
                                    format!("allow-{}", p.request_id),
                                ))
                                .xsmall()
                                .label("Allow")
                                .on_click(cx.listener(
                                    move |panel, _, _, cx| {
                                        panel.session.update(cx, |s, cx| s.answer(&rid1, true, cx))
                                    },
                                )),
                            )
                            .child(
                                gpui_kit::component::button::Button::new(SharedString::from(
                                    format!("deny-{}", p.request_id),
                                ))
                                .xsmall()
                                .label("Deny")
                                .on_click(cx.listener(
                                    move |panel, _, _, cx| {
                                        panel.session.update(cx, |s, cx| s.answer(&rid2, false, cx))
                                    },
                                )),
                            ),
                    );
            }
            Some(allowed) => {
                card = card.child(div().text_color(muted).child(if allowed {
                    "Allowed"
                } else {
                    "Denied"
                }));
            }
        }
        card.into_any_element()
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, hover, border, warning) = (
            theme.muted_foreground,
            theme.secondary_hover,
            theme.border,
            theme.warning,
        );
        let session = self.session.read(cx);
        let chat = &session.chat;
        let model = chat
            .model
            .clone()
            .or_else(|| session.model.clone())
            .unwrap_or_else(|| "default model".into());
        let model = model.trim_start_matches("claude-").to_string();
        let mode = session.permission_mode.clone();
        let busy = chat.busy;
        let mut status = vec![model];
        if let Some(s) = &chat.status {
            status.push(s.clone());
        }
        if let Some(c) = chat.last_turn_cost_usd {
            status.push(format!("turn ${c:.4}"));
        }
        if chat.total_cost_usd > 0.0 {
            status.push(format!("total ${:.4}", chat.total_cost_usd));
        }
        if let Some((i, o)) = chat.last_turn_tokens {
            status.push(format!("{}k in / {} out", i / 1000, o));
        }
        let accept = mode.as_deref() == Some("acceptEdits");
        div()
            .h(px(22.))
            .flex_none()
            .px_1()
            .flex()
            .items_center()
            .gap_1()
            .text_size(px(11.))
            .text_color(muted)
            .border_b_1()
            .border_color(border)
            .child(Icon::new(IconName::Bot).xsmall())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(status.join(" · ")),
            )
            .child(
                div()
                    .id("perm-mode")
                    .px_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(accept, |d| d.text_color(warning))
                    .hover(|d| d.bg(hover))
                    .child(if accept { "accept edits" } else { "ask first" })
                    .on_click(cx.listener(move |p, _, _, cx| {
                        let next = if accept {
                            None
                        } else {
                            Some("acceptEdits".to_string())
                        };
                        p.session
                            .update(cx, |s, cx| s.set_permission_mode(next, cx));
                    })),
            )
            .when(busy, |d| {
                d.child(
                    div()
                        .id("stop")
                        .px_1()
                        .rounded_sm()
                        .cursor_pointer()
                        .hover(|d| d.bg(hover))
                        .child("stop")
                        .on_click(
                            cx.listener(|p, _, _, cx| {
                                p.session.update(cx, |s, cx| s.interrupt(cx))
                            }),
                        ),
                )
            })
            .child(
                div()
                    .id("new-chat")
                    .px_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(|d| d.bg(hover))
                    .child("new")
                    .on_click(cx.listener(|p, _, _, cx| {
                        p.toggled.clear();
                        p.session.update(cx, |s, cx| s.new_conversation(cx))
                    })),
            )
            .into_any_element()
    }
}

impl Render for ClaudePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let started = std::time::Instant::now();
        let theme = cx.theme();
        let (fg, muted, red, user_bg, border) = (
            theme.foreground,
            theme.muted_foreground,
            theme.red,
            theme.secondary,
            theme.border,
        );
        let items = self.session.read(cx).chat.items.clone();
        let mut list = div().flex().flex_col().gap_2().px_2().py_2();
        if items.is_empty() {
            list = list.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("Claude Code runs in this folder. Ask for something; tool calls, diffs and permission prompts show here."),
            );
        }
        for (ix, item) in items.iter().enumerate() {
            let el = match item {
                Item::User(text) => div()
                    .p_2()
                    .rounded_md()
                    .bg(user_bg)
                    .text_sm()
                    .text_color(fg)
                    .child(text.clone())
                    .into_any_element(),
                Item::Assistant { text, .. } => div()
                    .text_color(fg)
                    .child(self.markdown(("msg", ix), text, cx))
                    .into_any_element(),
                Item::Tool(tool) => self.render_tool(ix, tool, cx),
                Item::Permission(p) => self.render_permission(p, cx),
                Item::Notice(t) => div()
                    .text_size(px(11.))
                    .text_color(muted)
                    .child(t.clone())
                    .into_any_element(),
                Item::Error(t) => div()
                    .text_xs()
                    .text_color(red)
                    .child(t.clone())
                    .into_any_element(),
            };
            list = list.child(el);
        }
        let scroll = self.scroll.clone();
        crate::perf::record("chat build (elements)", started.elapsed());
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.render_header(cx))
            .child(
                div()
                    .id("chat-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .on_scroll_wheel(cx.listener(move |p, _, _, _| {
                        // Follow new output only while the view is at the bottom.
                        let max = scroll.max_offset().y;
                        p.follow = scroll.offset().y <= -max + px(24.);
                    }))
                    .child(list),
            )
            .child(
                div()
                    .flex_none()
                    .p_1()
                    .border_t_1()
                    .border_color(border)
                    .child(Textarea::new(&self.input).text_sm()),
            )
    }
}
