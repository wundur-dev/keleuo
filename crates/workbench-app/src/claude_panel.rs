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
use workbench_core::conversation::{
    AgentStatus, Item, PermissionPrompt, SubAgent, ToolCall, is_agent_tool,
};
use workbench_core::diff::{FileDiff, LineKind};
use workbench_core::links::{self, FileIndex};
use workbench_core::summary::one_line;

use crate::claude_session::ClaudeSession;

pub enum ClaudePanelEvent {
    OpenFile(PathBuf, Option<u32>),
    NewSession,
    /// The session's tab was closed.
    Closed,
    /// Its tab became the shown one.
    Activated,
}

pub struct ClaudePanel {
    pub session: Entity<ClaudeSession>,
    /// The session's number and the key of its saved name.
    pub number: usize,
    pub name_key: String,
    focus: FocusHandle,
    input: Entity<TextareaState>,
    scroll: ScrollHandle,
    /// Tool cards the user opened (or closed, for diffs which start open).
    toggled: HashSet<String>,
    pub index: Arc<FileIndex>,
    follow: bool,
    last_revision: u64,
    /// How many of the latest items are rendered (long histories render lazily).
    shown: usize,
    /// Redraws once a second while subagents run (their elapsed time).
    ticker: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

/// Items rendered at first; "Show earlier" adds this many more.
const PAGE: usize = 150;

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
                let running = session.read(cx).chat.running_agents().next().is_some();
                if running && panel.ticker.is_none() {
                    panel.ticker = Some(cx.spawn(
                        async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                            loop {
                                cx.background_executor()
                                    .timer(std::time::Duration::from_secs(1))
                                    .await;
                                let alive = this.update(cx, |p, cx| {
                                    let still =
                                        p.session.read(cx).chat.running_agents().next().is_some();
                                    cx.notify();
                                    if !still {
                                        p.ticker = None;
                                    }
                                    still
                                });
                                if !matches!(alive, Ok(true)) {
                                    break;
                                }
                            }
                        },
                    ));
                }
                cx.notify();
            }
        });
        Self {
            session,
            number: 0,
            name_key: String::new(),
            focus: cx.focus_handle(),
            input,
            scroll: ScrollHandle::new(),
            toggled: HashSet::new(),
            index: Arc::new(FileIndex::default()),
            follow: true,
            last_revision: 0,
            shown: PAGE,
            ticker: None,
            _subs: vec![s1, s2],
        }
    }

    /// Keyboard focus is in this session (its message box or the panel).
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
            || self
                .input
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
    }

    pub fn focus_input(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Show a subagent's messages (from the sidebar tree or the strip).
    pub fn expand_agent(&mut self, tool_use_id: &str, cx: &mut Context<Self>) {
        self.toggled.insert(format!("agent:{tool_use_id}"));
        cx.notify();
    }

    fn agent_open(&self, tool_use_id: &str) -> bool {
        self.toggled.contains(&format!("agent:{tool_use_id}"))
    }

    fn toggle_agent(&mut self, tool_use_id: &str, cx: &mut Context<Self>) {
        let key = format!("agent:{tool_use_id}");
        if !self.toggled.remove(&key) {
            self.toggled.insert(key);
        }
        cx.notify();
    }

    fn open(&self, path: PathBuf, line: Option<u32>, cx: &mut Context<Self>) {
        cx.emit(ClaudePanelEvent::OpenFile(path, line));
    }

    fn markdown(&self, id: SharedString, text: &str, cx: &mut Context<Self>) -> AnyElement {
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
        if is_agent_tool(&tool.name)
            && let Some(agent) = self.session.read(cx).chat.agent(&tool.id).cloned()
        {
            return self.render_agent(&agent, true, cx);
        }
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

    /// Elements for a list of items; `key` keeps element ids unique per list.
    fn render_items(&self, items: &[Item], key: &str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (fg, muted, red, user_bg, accent) = (
            theme.foreground,
            theme.muted_foreground,
            theme.red,
            theme.secondary,
            theme.ring,
        );
        let mut out = Vec::new();
        for (ix, item) in items.iter().enumerate() {
            let el = match item {
                Item::User(text) => div()
                    .pl_2()
                    .pr_2()
                    .py_1()
                    .rounded_md()
                    .border_l_2()
                    .border_color(accent)
                    .bg(user_bg)
                    .text_sm()
                    .text_color(fg)
                    .child(text.clone())
                    .into_any_element(),
                Item::Assistant { text, .. } => div()
                    .text_color(fg)
                    .child(self.markdown(SharedString::from(format!("msg-{key}-{ix}")), text, cx))
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
            out.push(el);
        }
        out
    }

    /// One subagent: a compact row (status, description, type, elapsed, current tool, tokens);
    /// its own messages below when opened. `card` draws it as a card in the chat.
    fn render_agent(&self, agent: &SubAgent, card: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, hover, green, red, violet) = (
            theme.foreground,
            theme.muted_foreground,
            theme.border,
            theme.secondary_hover,
            theme.green,
            theme.red,
            theme.magenta,
        );
        let open = self.agent_open(&agent.tool_use_id);
        let id = agent.tool_use_id.clone();
        let status = match agent.status {
            AgentStatus::Running => Icon::new(IconName::LoaderCircle)
                .xsmall()
                .text_color(violet),
            AgentStatus::Done => Icon::new(IconName::Check).xsmall().text_color(green),
            AgentStatus::Failed => Icon::new(IconName::CircleX).xsmall().text_color(red),
        };
        let mut facts = Vec::new();
        if let Some(e) = agent.elapsed() {
            facts.push(format_elapsed(e));
        }
        if let Some(t) = &agent.current_tool {
            facts.push(t.clone());
        } else if let Some(n) = agent.tool_uses {
            facts.push(format!("{n} tools"));
        }
        if let Some(t) = agent.total_tokens {
            facts.push(format!("{:.1}k tok", t as f64 / 1000.0));
        }
        let title = if agent.description.is_empty() {
            agent.kind.clone()
        } else {
            agent.description.clone()
        };
        let header = div()
            .id(SharedString::from(format!(
                "agent-h-{}-{card}",
                agent.tool_use_id
            )))
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
                    .text_color(violet)
                    .child("agent"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(fg)
                    .child(title),
            )
            .when(!agent.kind.is_empty(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_color(muted)
                        .child(agent.kind.clone()),
                )
            })
            .child(div().flex_none().text_color(muted).child(facts.join(" · ")))
            .on_click(cx.listener(move |p, _, _, cx| p.toggle_agent(&id, cx)));
        let mut el = div()
            .flex()
            .flex_col()
            .border_l_2()
            .border_color(violet.opacity(0.7))
            .when(card, |d| {
                d.border_1()
                    .border_color(border)
                    .rounded_md()
                    .bg(violet.opacity(0.05))
            })
            .overflow_hidden()
            .child(header);
        if open {
            let mut body = div()
                .flex()
                .flex_col()
                .gap_1()
                .px_2()
                .py_1()
                .border_t_1()
                .border_color(border);
            if agent.items.is_empty() {
                body = body.child(div().text_xs().text_color(muted).child("No messages yet."));
            }
            for child in self.render_items(&agent.items.clone(), &agent.tool_use_id.clone(), cx) {
                body = body.child(child);
            }
            if let Some(summary) = &agent.summary {
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("Result: {}", one_line(summary, 400))),
                );
            }
            el = el.child(if card {
                body.into_any_element()
            } else {
                div()
                    .max_h(px(260.))
                    .id(SharedString::from(format!("agent-b-{}", agent.tool_use_id)))
                    .overflow_y_scroll()
                    .child(body)
                    .into_any_element()
            });
        }
        el.into_any_element()
    }

    /// The strip above the chat while this turn's subagents run.
    fn render_agent_strip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let chat = &self.session.read(cx).chat;
        chat.running_agents().next()?;
        let agents: Vec<SubAgent> = chat.turn_agents().to_vec();
        let theme = cx.theme();
        let (border, bg) = (theme.border, theme.secondary);
        let mut strip = div()
            .flex_none()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(border)
            .bg(bg.opacity(0.6));
        for a in &agents {
            strip = strip.child(self.render_agent(a, false, cx));
        }
        Some(strip.into_any_element())
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
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(Icon::new(IconName::Plus).xsmall())
                    .child("New session")
                    .tooltip(|w, cx| {
                        gpui_kit::component::tooltip::Tooltip::new("New Claude session (Cmd+N)")
                            .build(w, cx)
                    })
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ClaudePanelEvent::NewSession))),
            )
            .into_any_element()
    }
}

impl Focusable for ClaudePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ClaudePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let started = std::time::Instant::now();
        let focused = self.contains_focus(window, cx);
        let ring = cx.theme().ring;
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
        let hidden = items.len().saturating_sub(self.shown);
        if hidden > 0 {
            list = list.child(
                div()
                    .id("show-earlier")
                    .text_xs()
                    .text_color(muted)
                    .cursor_pointer()
                    .child(format!("Show {} earlier messages", hidden.min(PAGE)))
                    .on_click(cx.listener(|p, _, _, cx| {
                        p.shown += PAGE;
                        p.follow = false;
                        cx.notify();
                    })),
            );
        }
        let first = hidden;
        for el in self.render_items(&items[first..], &format!("main{first}"), cx) {
            list = list.child(el);
        }
        let _ = (fg, red, user_bg);
        let scroll = self.scroll.clone();
        crate::perf::record("chat build (elements)", started.elapsed());
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(gpui_kit::rgb(0x17181b))
            .track_focus(&self.focus)
            .border_t_2()
            .border_color(if focused { ring } else { transparent_black() })
            .child(self.render_header(cx))
            .children(self.render_agent_strip(cx))
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

/// `42s`, `3m 05s`.
fn format_elapsed(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else {
        format!("{}m {:02}s", s / 60, s % 60)
    }
}
