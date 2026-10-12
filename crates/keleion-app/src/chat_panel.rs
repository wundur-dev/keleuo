//! The chat panel: the conversation rendered natively from the agent's events. Assistant text as markdown,
//! tool calls as collapsible cards, file edits as diffs, permission prompts with Allow and
//! Deny, and a one-line status in the header. Every file reference is a link.

use std::collections::HashSet;
use std::rc::Rc;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};

use crate::agent_session::{Asking, Spend, View};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::DropdownMenu;
use crate::sheet_tabs::{self, Joined, Palette, StripState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleion_core::conversation::{
    AgentStatus, Item, PermissionPrompt, SubAgent, ToolCall,
};
use keleion_core::diff::{FileDiff, LineKind};
use keleion_core::links::{self, FileIndex};
use keleion_core::conversation::one_line;

use crate::agent_session::AgentSession;

pub enum ChatPanelEvent {
    OpenFile(PathBuf, Option<u32>),
    NewSession,
    /// The session's tab was closed.
    Closed,
    /// Its tab became the shown one.
    Activated,
}

pub struct ChatPanel {
    pub session: Entity<AgentSession>,
    /// The session's number (saved with the layout).
    pub number: usize,
    focus: FocusHandle,
    input: Entity<TextareaState>,
    /// The conversation as a virtual list: only the items on screen (and a margin) are built
    /// and laid out; it follows the end while the view is at the bottom.
    list: ListState,
    /// How many items the list knows of.
    list_len: usize,
    /// Earlier parts being read (by their start in the session file).
    loading: HashSet<u64>,
    /// Images to send with the next message.
    attachments: Vec<PathBuf>,
    /// An image shown full size over the chat.
    preview: Option<PathBuf>,
    /// Tool cards the user opened (or closed, for diffs which start open).
    toggled: HashSet<String>,
    pub index: Arc<FileIndex>,
    last_revision: u64,
    /// Redraws once a second while subagents run (their elapsed time).
    ticker: Option<Task<()>>,
    /// The Chat and Terminal view tabs.
    views: Rc<StripState>,
    _subs: Vec<Subscription>,
}

/// How far beyond the visible area the list builds items, so scrolling shows them at once.
const OVERDRAW: f32 = 600.;
/// Items at the end measured again on every change (text streaming in, a result arriving).
const TAIL: usize = 4;

impl EventEmitter<ChatPanelEvent> for ChatPanel {}

const MONO: &str = "Menlo";

impl ChatPanel {
    pub fn new(
        session: Entity<AgentSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = format!(
            "Message {} (Enter sends, Shift+Enter new line)",
            session.read(cx).backend.name()
        );
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 10)
                .placeholder(placeholder)
                .submit_on_enter(true)
        });
        let s1 = cx.subscribe_in(
            &input,
            window,
            |panel, input, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = ev {
                    let text = input.read(cx).value().to_string();
                    if text.trim().is_empty() && panel.attachments.is_empty() {
                        return;
                    }
                    input.update(cx, |s, cx| s.set_value("", window, cx));
                    // Sending goes back to the end and follows it.
                    panel.list.set_follow_mode(FollowMode::Tail);
                    let images = std::mem::take(&mut panel.attachments);
                    panel.session.update(cx, |s, cx| s.send_with(&text, images, cx));
                }
            },
        );
        let s2 = cx.observe(&session, |panel, session, cx| {
            let rev = session.read(cx).chat.revision;
            if rev != panel.last_revision {
                panel.last_revision = rev;
                panel.sync_list(session.read(cx).chat.items.len());
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
            }
            // The view (Chat or Terminal) may have changed too.
            cx.notify();
        });
        Self {
            session,
            number: 0,
            focus: cx.focus_handle(),
            input,
            list: {
                let list = ListState::new(0, ListAlignment::Top, px(OVERDRAW));
                list.set_follow_mode(FollowMode::Tail);
                list
            },
            list_len: 0,
            loading: HashSet::new(),
            attachments: Vec::new(),
            preview: None,
            toggled: HashSet::new(),
            index: Arc::new(FileIndex::default()),
            last_revision: 0,
            ticker: None,
            views: Rc::new(StripState::default()),
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

    /// Tests: scroll by `dy` pixels (down is positive). Every item is reachable already.
    pub fn test_command(&mut self, cmd: &str, dy: f32, cx: &mut Context<Self>) {
        match cmd {
            "scroll" => self.list.scroll_by(px(dy)),
            "top" => self.list.scroll_to(ListOffset { item_ix: 0, offset_in_item: px(0.) }),
            _ => {}
        }
        cx.notify();
    }

    /// Tell the list what changed: new items at the end, and the last few measured again
    /// (they may have grown). A shorter conversation (reloaded) starts over.
    fn sync_list(&mut self, len: usize) {
        if len < self.list_len {
            self.list.reset(len);
        } else {
            let from = self.list_len.saturating_sub(TAIL);
            self.list.splice(from..self.list_len, len - from);
        }
        self.list_len = len;
    }

    /// An earlier part came into view: read it off the UI thread, then put its items in its
    /// place, keeping the view where it was (on the part's last item).
    fn load_earlier(&mut self, from: u64, to: u64, cx: &mut Context<Self>) {
        // One part at a time: placeholders are one line each, so at the very top several are
        // in view together, but only the one the person reaches should be read.
        if !self.loading.is_empty() {
            return;
        }
        self.loading.insert(from);
        let Some(load) = self.session.read(cx).earlier_loader() else { return };
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let started = std::time::Instant::now();
            let items = cx.background_spawn(async move { load(from, to) }).await;
            let _ = this.update(cx, |p, cx| {
                p.loading.remove(&from);
                let Ok(items) = items else { return };
                if crate::perf::enabled() {
                    eprintln!("keleion: earlier part ({} items) read in {:.0} ms", items.len(), started.elapsed().as_secs_f64() * 1000.0);
                }
                let at = p.session.read(cx).chat.items.iter().position(|i| matches!(i, Item::Earlier { from: f, .. } if *f == from));
                let Some(ix) = at else { return };
                let n = items.len();
                let rev = p.session.update(cx, |s, _| {
                    s.chat.items.splice(ix..ix + 1, items);
                    s.chat.revision += 1;
                    s.chat.revision
                });
                let top = p.list.logical_scroll_top();
                p.list.splice(ix..ix + 1, n);
                p.list_len = p.list_len + n - 1;
                // Reached by scrolling up: carry on from its newest item. Jumped to the very top:
                // stay at its start.
                if top.item_ix == ix && ix > 0 && n > 0 {
                    p.list.scroll_to(ListOffset { item_ix: ix + n - 1, offset_in_item: px(0.) });
                }
                p.last_revision = rev;
                cx.notify();
            });
        })
        .detach();
    }

    /// The divider where the context was compacted, its summary on request.
    fn render_compacted(&self, summary: &str, trigger: Option<&str>, tokens: Option<u64>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, border, hover) = (theme.muted_foreground, theme.border, theme.secondary_hover);
        let key = format!("compact:{}:{}", summary.len(), summary.chars().take(40).collect::<String>());
        let open = self.toggled.contains(&key);
        let detail = match (trigger, tokens) {
            (Some(t), Some(n)) => format!(" ({t}, at {}k tokens)", n / 1000),
            (Some(t), None) => format!(" ({t})"),
            (None, Some(n)) => format!(" (at {}k tokens)", n / 1000),
            _ => String::new(),
        };
        let k = key.clone();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .id(SharedString::from(key))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(11.))
                    .text_color(muted)
                    .cursor_pointer()
                    .hover(|d| d.bg(hover))
                    .child(div().flex_1().h(px(1.)).bg(border))
                    .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall())
                    .child(format!("Context compacted here{detail}"))
                    .child(div().flex_1().h(px(1.)).bg(border))
                    .on_click(cx.listener(move |p, _, _, cx| {
                        if !p.toggled.remove(&k) {
                            p.toggled.insert(k.clone());
                        }
                        p.list.remeasure();
                        cx.notify();
                    })),
            )
            .when(open && !summary.is_empty(), |d| {
                d.child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(border)
                        .text_color(muted)
                        .child(self.markdown(SharedString::from(format!("summary-{}", summary.len())), summary, cx)),
                )
            })
            .into_any_element()
    }

    /// One item of the conversation, for the list.
    fn render_entry(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let item = self.session.read(cx).chat.items.get(ix).cloned();
        let Some(item) = item else { return div().into_any_element() };
        if let Item::Earlier { from, to, .. } = item {
            // In view (or about to be): read it.
            self.load_earlier(from, to, cx);
        }
        let last = ix + 1 == self.list_len;
        // A centred column of comfortable width, the page behind it.
        div()
            .w_full()
            .flex()
            .justify_center()
            .px_4()
            .pt_3()
            .when(last, |d| d.pb_4())
            .child(
                div()
                    .w_full()
                    .max_w(px(COLUMN))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(self.render_items(std::slice::from_ref(&item), &format!("main{ix}"), cx)),
            )
            .into_any_element()
    }

    pub fn focus_input(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Show a subagent's messages (from the sidebar tree or the strip).
    pub fn expand_agent(&mut self, tool_use_id: &str, cx: &mut Context<Self>) {
        self.toggled.insert(format!("agent:{tool_use_id}"));
        self.list.remeasure();
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
        // A card changed height: measure again (the scroll position stays on its item).
        self.list.remeasure();
        cx.notify();
    }

    fn open(&self, path: PathBuf, line: Option<u32>, cx: &mut Context<Self>) {
        cx.emit(ChatPanelEvent::OpenFile(path, line));
    }

    fn markdown(&self, id: SharedString, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let md = self.index.linkify_markdown(text);
        let entity = cx.entity().downgrade();
        TextView::markdown(id, md)
            .selectable(true)
            .text_sm()
            // Code blocks: the language and a copy button.
            .code_block_actions(|block, _, cx| {
                let code = block.code();
                let lang = block.lang().unwrap_or_else(|| "code".into());
                let muted = cx.theme().muted_foreground;
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(11.))
                    .text_color(muted)
                    .child(lang)
                    .child(
                        Button::new("copy-code")
                            .xsmall()
                            .ghost()
                            .icon(gpui_kit::assets::IconName::Copy)
                            .tooltip("Copy")
                            .on_click(move |_, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
                                gpui_kit::component::WindowExt::push_notification(window, "Copied", cx);
                            }),
                    )
            })
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
        // A tool call that started a subagent shows as the subagent's card.
        if let Some(agent) = self.session.read(cx).chat.agent(&tool.id).cloned()
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
                p.list.remeasure();
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
        let (fg, muted, red, user_bg, _accent) = (
            theme.foreground,
            theme.muted_foreground,
            theme.red,
            theme.secondary,
            theme.ring,
        );
        let mut out = Vec::new();
        for (ix, item) in items.iter().enumerate() {
            let el = match item {
                // The person's message: a rounded bubble on the right.
                Item::User(text) => {
                    let copy = text.clone();
                    let group = SharedString::from(format!("user-{key}-{ix}"));
                    div()
                        .group(group.clone())
                        .w_full()
                        .flex()
                        .flex_col()
                        .items_end()
                        .gap_1()
                        .child(
                            div()
                                .max_w(relative(0.8))
                                .px_3()
                                .py_2()
                                .rounded(px(18.))
                                .bg(user_bg)
                                .text_sm()
                                .line_height(rems(1.45))
                                .text_color(fg)
                                .child(text.clone()),
                        )
                        .child(hover_actions(group, format!("{key}-{ix}"), copy, muted))
                        .into_any_element()
                }
                // Claude's reply: plain text across the column, no bubble.
                Item::Assistant { text, streaming } => {
                    let copy = text.clone();
                    let group = SharedString::from(format!("reply-{key}-{ix}"));
                    div()
                        .group(group.clone())
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_color(fg)
                        .line_height(rems(1.5))
                        .child(self.markdown(SharedString::from(format!("msg-{key}-{ix}")), text, cx))
                        .when(!streaming, |d| d.child(hover_actions(group, format!("{key}-{ix}"), copy, muted)))
                        .into_any_element()
                }
                Item::Images(paths) => {
                    let mut row = div().w_full().flex().justify_end().gap_2().flex_wrap();
                    for (n, path) in paths.iter().enumerate() {
                        let p = path.clone();
                        row = row.child(
                            div()
                                .id(SharedString::from(format!("img-{key}-{ix}-{n}")))
                                .size(px(96.))
                                .rounded(px(12.))
                                .overflow_hidden()
                                .border_1()
                                .border_color(cx.theme().border)
                                .cursor_pointer()
                                .child(img(path.clone()).size_full().object_fit(ObjectFit::Cover))
                                .on_click(cx.listener(move |panel, _, _, cx| {
                                    panel.preview = Some(p.clone());
                                    cx.notify();
                                })),
                        );
                    }
                    row.into_any_element()
                }
                Item::Tool(tool) => self.render_tool(ix, tool, cx),
                Item::Permission(p) => self.render_permission(p, cx),
                Item::Notice(t) => div()
                    .text_size(px(11.))
                    .text_color(muted)
                    .child(t.clone())
                    .into_any_element(),
                Item::Earlier { messages, .. } => div()
                    .text_size(px(11.))
                    .text_color(muted)
                    .child(format!("Earlier conversation, {messages} messages: reading..."))
                    .into_any_element(),
                Item::Compacted { summary, trigger, tokens } => {
                    self.render_compacted(summary, trigger.as_deref(), *tokens, cx)
                }
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
        let (muted, hover, border) = (theme.muted_foreground, theme.secondary_hover, theme.border);
        let session = self.session.read(cx);
        let chat = &session.chat;
        let model = chat
            .model
            .clone()
            .or_else(|| session.model.clone())
            .unwrap_or_else(|| "default model".into());
        let mut status = vec![model];
        if let Some(s) = &chat.status {
            status.push(s.clone());
        }
        // Cost when billed per use (or the setting says so), else how full the context is.
        let setting = cx.global::<crate::AppSettings>().0.borrow().cost_display.clone();
        let (spend, spend_tip) = match session.spend(cx) {
            Some(Spend::Cost { total, turn, estimated }) => {
                // "~": worked out from token counts and published prices.
                let est = if estimated { "~" } else { "" };
                let turn = turn.map(|c| format!("turn {est}${c:.4} · ")).unwrap_or_default();
                let tokens = chat.last_turn_tokens.map(|(i, o)| format!(" · {}k in / {o} out", i / 1000)).unwrap_or_default();
                (
                    Some(format!("{turn}total {est}${total:.4}{tokens}")),
                    if estimated { "Estimated from token counts and published per-token prices" } else { "As the agent reported it" },
                )
            }
            Some(Spend::Context { percent, tokens, window }) => (
                Some(format!("{percent}% of context")),
                if tokens > 0 && window > 0 { "Tokens the last turn sent, against the model's context window" } else { "" },
            ),
            None => (None, ""),
        };
        let spend_tip = format!(
            "{spend_tip}. Cost shows: {}. Click to change.",
            match setting.as_str() {
                "always" => "always",
                "never" => "never (context instead)",
                _ => "auto (only when billed per use)",
            }
        );
        let backend_name = session.backend.name();
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
            .when_some(spend, |d, spend| {
                d.child(
                    div()
                        .id("spend")
                        .flex_none()
                        .px_1()
                        .rounded_sm()
                        .cursor_pointer()
                        .hover(|d| d.bg(hover))
                        .child(spend)
                        .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(spend_tip.clone()).build(w, cx))
                        .on_click(|_, window, cx| {
                            // Auto, then always, then never.
                            let settings = cx.global::<crate::AppSettings>().0.clone();
                            let next = match settings.borrow().cost_display.as_str() {
                                "always" => "never",
                                "never" => "auto",
                                _ => "always",
                            };
                            settings.borrow_mut().cost_display = next.into();
                            let _ = settings.borrow().save();
                            window.refresh();
                        }),
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
                    .tooltip(move |w, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(format!("New {backend_name} session (Cmd+N)"))
                            .build(w, cx)
                    })
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatPanelEvent::NewSession))),
            )
            .into_any_element()
    }
}

impl ChatPanel {
    /// The Chat and Terminal tabs along the bottom, for backends with a terminal interface.
    fn render_views(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.session.read(cx);
        if !session.backend.capabilities().terminal_view {
            return None;
        }
        let theme = cx.theme();
        let ground = theme.sidebar;
        let content = theme.background;
        let palette = Palette::new(ground, theme.foreground, content, theme.foreground, theme.border, theme.ring);
        let shown = match session.view {
            View::Chat => 0,
            View::Terminal => 1,
        };
        let default_terminal = cx.global::<crate::AppSettings>().0.borrow().view_for(None) == "terminal";
        let muted = theme.muted_foreground;
        let weak = self.session.downgrade();
        let input = self.input.clone();
        let strip = sheet_tabs::simple_strip(
            &self.views,
            "session-view",
            &["Chat".into(), "Terminal".into()],
            shown,
            Joined::Top,
            palette,
            24.,
            Rc::new(move |ix, window, cx| {
                let view = if ix == 0 { View::Chat } else { View::Terminal };
                let _ = weak.update(cx, |s, cx| s.set_view(view, cx));
                // Typing goes where the view is: the terminal, or the message box.
                let term = weak.upgrade().and_then(|s| s.read(cx).terminal_view());
                match (view, term) {
                    (View::Terminal, Some(t)) => {
                        let f = t.read(cx).focus.clone();
                        window.focus(&f, cx)
                    }
                    _ => input.update(cx, |i, cx| i.focus(window, cx)),
                }
            }),
        );
        Some(
            strip
                .child(
                    div()
                        .id("default-view")
                        .px_2()
                        .text_size(px(10.5))
                        .text_color(muted)
                        .cursor_pointer()
                        .hover(|d| d.text_color(theme.foreground))
                        .child(if default_terminal { "New sessions open in Terminal" } else { "New sessions open in Chat" })
                        .tooltip(|w, cx| {
                            gpui_kit::component::tooltip::Tooltip::new("Click to change which view new sessions open in").build(w, cx)
                        })
                        .on_click(move |_, window, cx| {
                            let settings = cx.global::<crate::AppSettings>().0.clone();
                            let next = if default_terminal { "chat" } else { "terminal" };
                            settings.borrow_mut().default_view = next.into();
                            let _ = settings.borrow().save();
                            window.refresh();
                        }),
                )
                .into_any_element(),
        )
    }

    /// The interface waits on a question before it is up: Trust / Don't trust for the folder
    /// question, else a pointer to the terminal.
    fn render_asking(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.session.read(cx);
        let asking = session.asking?;
        if session.view == View::Terminal {
            return None;
        }
        let name = session.backend.name();
        let theme = cx.theme();
        let (warning, border, muted) = (theme.warning, theme.border, theme.muted_foreground);
        let weak = self.session.downgrade();
        let show = {
            let weak = weak.clone();
            Button::new("asking-show").xsmall().label("Show terminal").on_click(move |_, window, cx| {
                let _ = weak.update(cx, |s, cx| s.set_view(View::Terminal, cx));
                if let Some(t) = weak.upgrade().and_then(|s| s.read(cx).terminal_view()) {
                    let f = t.read(cx).focus.clone();
                    window.focus(&f, cx);
                }
            })
        };
        let mut row = div()
            .flex_none()
            .px_2()
            .py_1()
            .flex()
            .items_center()
            .gap_2()
            .border_t_1()
            .border_color(border)
            .text_size(px(11.5))
            .child(Icon::new(IconName::TriangleAlert).xsmall().text_color(warning));
        row = match asking {
            Asking::Trust => {
                let (w1, w2) = (weak.clone(), weak.clone());
                row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(format!("{name} asks whether you trust this folder: it will be able to read, edit and run files here."))
                        .child(div().text_color(muted).child("Your answer goes to its own prompt, as if typed.")),
                )
                .child(Button::new("trust").xsmall().primary().label("Trust").on_click(move |_, _, cx| {
                    let _ = w1.update(cx, |s, cx| s.answer_trust(true, cx));
                }))
                .child(Button::new("distrust").xsmall().label("Don't trust").on_click(move |_, _, cx| {
                    let _ = w2.update(cx, |s, cx| s.answer_trust(false, cx));
                }))
                .child(show)
            }
            Asking::Other => row.child(div().flex_1().min_w_0().child(format!("{name} is asking something in the terminal."))).child(show),
        };
        Some(row.into_any_element())
    }

    /// In the terminal view, the files the agent changed this session, as links.
    fn render_edits(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let chat = &self.session.read(cx).chat;
        let mut seen: Vec<(PathBuf, usize, usize)> = Vec::new();
        for item in chat.items.iter().rev() {
            if let Item::Tool(t) = item
                && let Some(d) = &t.diff
                && t.done()
                && !t.is_error
                && !seen.iter().any(|(p, ..)| *p == d.path)
            {
                seen.push((d.path.clone(), d.added(), d.removed()));
            }
            if seen.len() >= 6 {
                break;
            }
        }
        if seen.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let (muted, link, green, red, border) = (theme.muted_foreground, theme.blue, theme.green, theme.red, theme.border);
        let root = PathBuf::from(&chat.cwd);
        let mut row = div()
            .flex_none()
            .h(px(22.))
            .px_2()
            .flex()
            .items_center()
            .gap_3()
            .border_t_1()
            .border_color(border)
            .text_size(px(11.))
            .text_color(muted)
            .child("Edited:");
        for (i, (path, added, removed)) in seen.into_iter().enumerate() {
            let name = path.strip_prefix(&root).unwrap_or(&path).display().to_string();
            row = row.child(
                div()
                    .id(("edited", i))
                    .flex()
                    .gap_1()
                    .cursor_pointer()
                    .child(div().text_color(link).child(name))
                    .child(div().text_color(green).child(format!("+{added}")))
                    .child(div().text_color(red).child(format!("-{removed}")))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(ChatPanelEvent::OpenFile(path.clone(), None)))),
            );
        }
        Some(row.into_any_element())
    }
}

impl Focusable for ChatPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ChatPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let started = std::time::Instant::now();
        if crate::perf::enabled() {
            // Build, layout and paint: from here to the frame being on screen.
            window.on_next_frame(move |_, _| crate::perf::record("chat frame (render to frame)", started.elapsed()));
        }
        let focused = self.contains_focus(window, cx);
        let ring = cx.theme().ring;
        let theme = cx.theme();
        let (fg, muted, red, user_bg, _border) = (
            theme.foreground,
            theme.muted_foreground,
            theme.red,
            theme.secondary,
            theme.border,
        );
        let backend_name = self.session.read(cx).backend.name();
        if self.list_len != self.session.read(cx).chat.items.len() {
            self.sync_list(self.session.read(cx).chat.items.len());
        }
        let empty = self.list_len == 0;
        let _ = (fg, red, user_bg);
        let terminal = {
            let s = self.session.read(cx);
            (s.view == View::Terminal).then(|| s.terminal_view()).flatten()
        };
        crate::perf::record("chat build (elements)", started.elapsed());
        crate::perf::switch_shown(window);
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .track_focus(&self.focus)
            .border_t_2()
            .border_color(if focused { ring } else { transparent_black() })
            .child(self.render_header(cx))
            .children(self.render_agent_strip(cx))
            .when_some(terminal.clone(), |d, term| {
                d.child(div().flex_1().min_h_0().relative().child(term))
                    .children(self.render_edits(cx))
            })
            .when(terminal.is_none(), |d| d.child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when(empty, |d| d.p_2().child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("{backend_name} runs in this folder. Ask for something; tool calls, diffs and permission prompts show here.")),
                    ))
                    .when(!empty, |d| d.child(
                        list(self.list.clone(), cx.processor(|this, ix, _window, cx| this.render_entry(ix, cx)))
                            .size_full(),
                    )),
            )
            .children(self.render_asking(cx))
            .child(self.render_composer(window, cx)))
            .children(self.render_views(cx))
            .children(self.preview.clone().map(|path| {
                div()
                    .id("image-preview")
                    .absolute()
                    .inset_0()
                    .bg(gpui_kit::black().opacity(0.8))
                    .flex()
                    .items_center()
                    .justify_center()
                    .p_6()
                    .cursor_pointer()
                    .child(img(path).max_w_full().max_h_full().object_fit(ObjectFit::Contain))
                    .on_click(cx.listener(|p, _, _, cx| {
                        p.preview = None;
                        cx.notify();
                    }))
            }))
            .relative()
    }
}

impl ChatPanel {
    /// Tests: send `text` with what is attached.
    pub fn test_send(&mut self, text: &str, cx: &mut Context<Self>) {
        let images = std::mem::take(&mut self.attachments);
        self.session.update(cx, |s, cx| s.send_with(text, images, cx));
    }

    /// Tests: attach an image file.
    pub fn test_attach(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.attach([path], cx);
    }

    /// Add image files to the next message.
    fn attach(&mut self, paths: impl IntoIterator<Item = PathBuf>, cx: &mut Context<Self>) {
        for p in paths {
            let image = matches!(
                p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
                Some("png" | "jpg" | "jpeg" | "gif" | "webp")
            );
            if image && !self.attachments.contains(&p) {
                self.attachments.push(p);
            }
        }
        cx.notify();
    }

    /// A pasted image (bytes from the clipboard): kept in the session's image folder.
    fn attach_bytes(&mut self, bytes: &[u8], ext: &str, cx: &mut Context<Self>) {
        let id = self.session.read(cx).chat.session_id.clone();
        if let Some(dir) = keleion_core::images::session_dir(id.as_deref())
            && let Ok(path) = keleion_core::images::store(&dir, bytes, ext)
        {
            self.attach([path], cx);
        }
    }

    /// The composer: a rounded box floating at the bottom of the column, with the images to
    /// send, the text, and the session's controls (attach, model, permission mode, send/stop).
    fn render_composer(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (border, muted, fg, field, accent) = (theme.border, theme.muted_foreground, theme.foreground, theme.secondary, theme.ring);
        let session = self.session.read(cx);
        let busy = session.chat.busy;
        let caps = session.backend.capabilities();
        let model = session.chat.model.clone().or_else(|| session.model.clone()).unwrap_or_else(|| "default model".into());
        let mode = session.live_mode.clone().or_else(|| session.permission_mode.clone()).unwrap_or_else(|| "default".into());
        let has_text = !self.input.read(cx).value().trim().is_empty() || !self.attachments.is_empty();
        let weak = self.session.downgrade();
        let entity = cx.entity().downgrade();

        let mut chips = div().flex().flex_wrap().gap_2();
        for (n, path) in self.attachments.iter().enumerate() {
            let (p1, p2) = (path.clone(), path.clone());
            chips = chips.child(
                div()
                    .id(("chip", n))
                    .relative()
                    .size(px(56.))
                    .rounded(px(10.))
                    .overflow_hidden()
                    .border_1()
                    .border_color(border)
                    .cursor_pointer()
                    .child(img(path.clone()).size_full().object_fit(ObjectFit::Cover))
                    .on_click(cx.listener(move |p, _, _, cx| {
                        p.preview = Some(p1.clone());
                        cx.notify();
                    }))
                    .child(
                        div()
                            .id(("chip-x", n))
                            .absolute()
                            .top(px(2.))
                            .right(px(2.))
                            .size(px(16.))
                            .rounded_full()
                            .bg(gpui_kit::black().opacity(0.7))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(IconName::Close).xsmall().text_color(gpui_kit::white()))
                            .on_click(cx.listener(move |p, _, _, cx| {
                                cx.stop_propagation();
                                p.attachments.retain(|a| *a != p2);
                                cx.notify();
                            })),
                    ),
            );
        }

        let models: Vec<String> = caps.models.clone();
        let (w_model, w_mode) = (weak.clone(), weak.clone());
        let model_button = Button::new("model")
            .xsmall()
            .ghost()
            .label(short_model(&model))
            .tooltip("Model")
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                for m in &models {
                    let (w, m2) = (w_model.clone(), m.clone());
                    menu = menu.item(gpui_kit::component::menu::PopupMenuItem::new(capitalize(m)).on_click(move |_, _, cx| {
                        let _ = w.update(cx, |s, cx| s.set_model(&m2, cx));
                    }));
                }
                menu
            });
        let modes: Vec<(Option<String>, &'static str)> = vec![
            (None, "Ask before acting"),
            (caps.accept_edits_mode.clone(), "Accept edits"),
            (Some("plan".to_string()), "Plan"),
        ];
        let mode_label = match mode.as_str() {
            "acceptEdits" => "Accept edits",
            "plan" => "Plan",
            "auto" => "Auto",
            "bypassPermissions" => "Bypass permissions",
            _ => "Ask before acting",
        };
        let mode_button = Button::new("mode")
            .xsmall()
            .ghost()
            .label(mode_label)
            .tooltip("Permission mode")
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                for (m, label) in &modes {
                    let (w, m2) = (w_mode.clone(), m.clone());
                    menu = menu.item(gpui_kit::component::menu::PopupMenuItem::new(*label).on_click(move |_, _, cx| {
                        let _ = w.update(cx, |s, cx| s.set_mode(m2.clone(), cx));
                    }));
                }
                menu
            });
        let input = self.input.clone();
        let send = div()
            .id("send")
            .size(px(28.))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .when(busy, |d| d.bg(fg))
            .when(!busy && has_text, |d| d.bg(fg))
            .when(!busy && !has_text, |d| d.bg(muted.opacity(0.35)))
            .child(
                Icon::new(if busy { gpui_kit::assets::IconName::Square } else { gpui_kit::assets::IconName::ArrowUp })
                    .small()
                    .text_color(theme.background),
            )
            .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(if busy { "Stop (Esc in the terminal)" } else { "Send (Enter)" }).build(w, cx))
            .on_click(cx.listener(move |p, _, window, cx| {
                if busy {
                    p.session.update(cx, |s, cx| s.interrupt(cx));
                    return;
                }
                let text = input.read(cx).value().to_string();
                if text.trim().is_empty() && p.attachments.is_empty() {
                    return;
                }
                input.update(cx, |s, cx| s.set_value("", window, cx));
                p.list.set_follow_mode(FollowMode::Tail);
                let images = std::mem::take(&mut p.attachments);
                p.session.update(cx, |s, cx| s.send_with(&text, images, cx));
            }));
        let attach = Button::new("attach")
            .xsmall()
            .ghost()
            .icon(gpui_kit::assets::IconName::Paperclip)
            .tooltip("Attach images")
            .on_click(cx.listener(|_, _, _, cx| {
                let paths = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
                cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    if let Ok(Ok(Some(paths))) = paths.await {
                        let _ = this.update(cx, |p, cx| p.attach(paths, cx));
                    }
                })
                .detach();
            }));
        let paste_target = entity.clone();
        div()
            .flex_none()
            .w_full()
            .flex()
            .justify_center()
            .px_4()
            .pb_3()
            .pt_1()
            .child(
                div()
                    .id("composer")
                    .w_full()
                    .max_w(px(COLUMN))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .rounded(px(20.))
                    .border_1()
                    .border_color(border)
                    .bg(field)
                    .shadow_md()
                    .drag_over::<ExternalPaths>(move |d, _, _, _| d.border_color(accent))
                    .on_drop(cx.listener(|p, paths: &ExternalPaths, _, cx| p.attach(paths.paths().to_vec(), cx)))
                    .when(!self.attachments.is_empty(), |d| d.child(chips))
                    .child(
                        Textarea::new(&self.input)
                            .appearance(false)
                            .text_sm()
                            .on_paste(move |item, _, cx| {
                                // An image (or image files) on the clipboard: attach it.
                                let mut took = false;
                                for entry in item.entries() {
                                    match entry {
                                        ClipboardEntry::Image(image) => {
                                            let ext = match image.format {
                                                ImageFormat::Jpeg => "jpg",
                                                ImageFormat::Gif => "gif",
                                                ImageFormat::Webp => "webp",
                                                _ => "png",
                                            };
                                            let bytes = image.bytes.clone();
                                            let _ = paste_target.update(cx, |p, cx| p.attach_bytes(&bytes, ext, cx));
                                            took = true;
                                        }
                                        ClipboardEntry::ExternalPaths(paths) => {
                                            let paths = paths.paths().to_vec();
                                            let _ = paste_target.update(cx, |p, cx| p.attach(paths, cx));
                                            took = true;
                                        }
                                        _ => {}
                                    }
                                }
                                took
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_color(muted)
                            .child(attach)
                            .child(model_button)
                            .when(caps.permission_prompts, |d| d.child(mode_button))
                            .child(div().flex_1())
                            .child(send),
                    ),
            )
            .into_any_element()
    }
}

/// The column's widest.
const COLUMN: f32 = 760.;

/// `claude-opus-5-5` as `opus-5-5`; a short name stays.
fn short_model(model: &str) -> String {
    model.strip_prefix("claude-").unwrap_or(model).to_string()
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// Copy, under a message, shown while the pointer is over it.
fn hover_actions(group: SharedString, key: String, text: String, muted: Hsla) -> impl IntoElement {
    div()
        .flex()
        .gap_1()
        .invisible()
        .group_hover(group, |d| d.visible())
        .child(
            Button::new(SharedString::from(format!("copy-{key}")))
                .xsmall()
                .ghost()
                .icon(gpui_kit::assets::IconName::Copy)
                .text_color(muted)
                .tooltip("Copy")
                .on_click(move |_, window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                    gpui_kit::component::WindowExt::push_notification(window, "Copied", cx);
                }),
        )
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
