//! The chat panel: the conversation rendered natively from the agent's events. Assistant text as markdown,
//! tool calls as collapsible cards, file edits as diffs, permission prompts with Allow and
//! Deny, and a one-line status in the header. Every file reference is a link.

use std::collections::HashSet;
use std::rc::Rc;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::input::{Enter, Escape, IndentInline, MoveDown, MoveUp, InlineToken, InlineTokenContext, Input, InputContent, InputEvent, InputState, Textarea, TextareaState};

use crate::agent_session::{Asking, Spend, View};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::DropdownMenu;
use crate::sheet_tabs::{self, Joined, Palette, StripState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleuo_core::conversation::{
    AgentStatus, Item, PermissionPrompt, SubAgent, ToolCall,
};
use keleuo_core::agent::Issue;
use keleuo_core::diff::{FileDiff, LineKind};
use keleuo_core::links::{self, FileIndex};
use keleuo_core::compose::{self, ListBreak};
use keleuo_core::paste;
use keleuo_core::conversation::{one_line, to_markdown};

use crate::actions::CopyConversation;
use crate::agent_session::AgentSession;

pub enum ChatPanelEvent {
    OpenFile(PathBuf, Option<u32>),
    /// Open the file as a diff of what the session changed in it.
    OpenDiff(PathBuf),
    NewSession,
    /// The session's tab was closed.
    Closed,
    /// Its tab became the shown one.
    Activated,
    /// Watch a subagent (by its tool call id) in its own docked panel.
    OpenAgent(String),
    /// Show a web page in the Web panel.
    OpenWeb(String),
    /// Run a shell command in a terminal tab of the session's folder.
    RunCommand(String),
}

pub struct ChatPanel {
    pub session: Entity<AgentSession>,
    /// The session's number (saved with the layout).
    pub number: usize,
    /// Set when this panel watches one subagent (its tool call id): it shows that subagent's
    /// items instead of the conversation, with no message box.
    pub agent: Option<String>,
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
    /// Long pastes in the message box, shown there as tokens (`[Pasted text #1 · …]`, see
    /// [`keleuo_core::paste`]); paste `n` is `pastes[n - 1]`.
    pastes: Vec<String>,
    /// "Other model…": a field for a model name in place of the model button.
    model_input: Option<(Entity<InputState>, Subscription)>,
    /// What a message starting with `/` can name (skills, commands, the agent's own), read
    /// when the first `/` is typed.
    slash_choices: Option<Rc<Vec<keleuo_core::slash::Choice>>>,
    /// The highlighted one in the `/` list, and the query it was for.
    slash_ix: usize,
    slash_query: String,
    /// Escape closed the list for this query.
    slash_closed: bool,
    /// An image shown full size over the chat.
    preview: Option<PathBuf>,
    /// The known failure whose fixes are being offered.
    fixing: Option<Issue>,
    /// Kinds of failure fixed from this panel (their notices say so).
    fixed: std::collections::HashSet<String>,
    /// Tool cards the user opened (or closed, for diffs which start open).
    toggled: HashSet<String>,
    pub index: Arc<FileIndex>,
    last_revision: u64,
    /// Redraws once a second while subagents or tool calls run (their elapsed time).
    ticker: Option<Task<()>>,
    /// When the agent became busy with the current turn (for the time on the thinking line).
    busy_since: Option<std::time::Instant>,
    /// The step the spinners are at: the ticker moves it on while something runs.
    phase: u32,
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
/// How often the chat redraws while something runs: the spinners' step and the clocks.
/// Ten a second reads as motion; continuous animation would redraw every frame.
const TICK: std::time::Duration = std::time::Duration::from_millis(100);
/// Steps in a spinner's turn (one turn a little over a second at the tick above).
const SPINNER_STEPS: u32 = 12;
/// How many of a running command's latest output lines its card shows.
const LIVE_LINES: usize = 10;
/// Space below the Chat / Terminal tabs, before the window's bottom edge.
const VIEW_STRIP_GAP_BELOW: f32 = 6.;

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
            |panel, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = ev {
                    panel.send_composed(window, cx);
                }
            },
        );
        let s2 = cx.observe(&session, |panel, session, cx| {
            match (session.read(cx).chat.busy, panel.busy_since) {
                (true, None) => panel.busy_since = Some(std::time::Instant::now()),
                (false, Some(_)) => panel.busy_since = None,
                _ => {}
            }
            let rev = session.read(cx).chat.revision;
            if rev != panel.last_revision {
                panel.last_revision = rev;
                let len = panel.item_count(cx);
                panel.sync_list(len);
                let running = clocks_running(&session.read(cx).chat);
                if running && panel.ticker.is_none() {
                    panel.ticker = Some(cx.spawn(
                        async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                            loop {
                                cx.background_executor().timer(TICK).await;
                                let alive = this.update(cx, |p, cx| {
                                    let still = clocks_running(&p.session.read(cx).chat);
                                    p.phase = p.phase.wrapping_add(1);
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
            agent: None,
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
            pastes: Vec::new(),
            model_input: None,
            slash_choices: None,
            slash_ix: 0,
            slash_query: String::new(),
            slash_closed: false,
            preview: None,
            fixing: None,
            fixed: Default::default(),
            toggled: HashSet::new(),
            index: Arc::new(FileIndex::default()),
            last_revision: 0,
            busy_since: None,
            phase: 0,
            ticker: None,
            views: Rc::new(StripState::default()),
            _subs: vec![s1, s2],
        }
    }

    /// A panel that watches one subagent of the session.
    pub fn new_for_agent(session: Entity<AgentSession>, tool_use_id: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut panel = Self::new(session, window, cx);
        panel.agent = Some(tool_use_id);
        panel
    }

    /// The items shown: the conversation, or the watched subagent's.
    fn item_count(&self, cx: &App) -> usize {
        let chat = &self.session.read(cx).chat;
        match &self.agent {
            Some(id) => chat.agent(id).map_or(0, |a| a.items.len()),
            None => chat.items.len(),
        }
    }

    fn item_at(&self, ix: usize, cx: &App) -> Option<Item> {
        let chat = &self.session.read(cx).chat;
        match &self.agent {
            Some(id) => chat.agent(id)?.items.get(ix).cloned(),
            None => chat.items.get(ix).cloned(),
        }
    }

    /// The line on a tool card that failed in a known way.
    fn render_issue_notice(&self, tool_id: &str, issue: &Issue, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, warning, green) = (theme.foreground, theme.muted_foreground, theme.border, theme.warning, theme.green);
        let fixed = self.fixed.contains(&issue.kind);
        let open = issue.clone();
        div()
            .px_2()
            .py_1()
            .border_t_1()
            .border_color(border)
            .bg(warning.opacity(0.08))
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .child(Icon::new(IconName::TriangleAlert).xsmall().text_color(warning))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .gap_1()
                    .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(issue.title.clone()))
                    .child(div().min_w_0().truncate().text_color(muted).child(issue.detail.replace('`', ""))),
            )
            .child(if fixed {
                div().flex_none().text_color(green).child("Fixed").into_any_element()
            } else {
                Button::new(SharedString::from(format!("fix-{tool_id}")))
                    .xsmall()
                    .label("Fix so it doesn't happen again")
                    .on_click(cx.listener(move |p, _, _, cx| {
                        cx.stop_propagation();
                        p.fixing = Some(open.clone());
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .into_any_element()
    }

    /// The fixes for a known failure, each saying exactly what it writes where.
    fn render_fix(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let issue = self.fixing.as_ref()?;
        let theme = cx.theme();
        let (fg, muted, border, bg) = (theme.foreground, theme.muted_foreground, theme.border, theme.background);
        let mut list = div().flex().flex_col().gap_2();
        for fix in &issue.fixes {
            let (kind, id) = (issue.kind.clone(), fix.id.clone());
            list = list.child(
                div()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(fix.label.clone()))
                            .child(div().text_xs().line_height(rems(1.4)).text_color(muted).child(fix.preview.clone())),
                    )
                    .child(
                        div().flex_none().child(
                            Button::new(SharedString::from(format!("apply-{id}")))
                                .small()
                                .primary()
                                .label("Apply")
                                .on_click(cx.listener(move |p, _, window, cx| {
                                    let result = p.session.read(cx).apply_fix(&kind, &id);
                                    match result {
                                        Ok(done) => {
                                            p.fixed.insert(kind.clone());
                                            p.fixing = None;
                                            gpui_kit::component::WindowExt::push_notification(window, done, cx);
                                        }
                                        Err(e) => gpui_kit::component::WindowExt::push_notification(window, format!("Not fixed: {e}"), cx),
                                    }
                                    p.list.remeasure();
                                    cx.notify();
                                })),
                        ),
                    ),
            );
        }
        Some(
            div()
                .id("fix-dialog")
                .absolute()
                .inset_0()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(60.))
                .bg(gpui_kit::black().opacity(0.25))
                .occlude()
                .capture_key_down(cx.listener(|p, ev: &KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        p.fixing = None;
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
                .child(
                    div()
                        .w(px(520.))
                        .max_w(relative(0.92))
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .rounded_lg()
                        .border_1()
                        .border_color(border)
                        .bg(bg)
                        .shadow_lg()
                        .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(issue.title.clone()))
                        .child(div().text_sm().line_height(rems(1.45)).text_color(muted).child(issue.detail.replace('`', "")))
                        .child(div().text_xs().text_color(muted).child("Choose a fix. It goes into the agent's own settings, so it works outside Keleuo too."))
                        .child(list)
                        .child(
                            div().flex().justify_end().child(
                                Button::new("fix-cancel").small().ghost().label("Not now").on_click(cx.listener(|p, _, _, cx| {
                                    p.fixing = None;
                                    cx.notify();
                                })),
                            ),
                        ),
                )
                .into_any_element(),
        )
    }

    /// A spinner at the panel's step: turned by the ticker, a few steps a second, rather
    /// than animated every frame (an animation redraws the whole chat on every frame).
    fn spinner(&self, color: Hsla) -> Icon {
        let turn = (self.phase % SPINNER_STEPS) as f32 / SPINNER_STEPS as f32;
        Icon::new(IconName::LoaderCircle).text_color(color).transform(Transformation::rotate(percentage(turn)))
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
                    eprintln!("keleuo: earlier part ({} items) read in {:.0} ms", items.len(), started.elapsed().as_secs_f64() * 1000.0);
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

    /// The whole conversation (or the watched subagent's) on the clipboard as Markdown. Parts
    /// not read yet are read first, off the UI thread, for the copy only: the chat stays as it is.
    pub fn copy_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session.read(cx);
        let name = session.backend.name();
        let items = match &self.agent {
            Some(id) => session.chat.agent(id).map(|a| a.items.clone()).unwrap_or_default(),
            None => session.chat.items.clone(),
        };
        let parts: Vec<_> = items
            .iter()
            .enumerate()
            .filter_map(|(ix, i)| match i {
                Item::Earlier { from, to, .. } => session.earlier_loader().map(|load| (ix, *from, *to, load)),
                _ => None,
            })
            .collect();
        cx.spawn_in(window, async move |_, cx| {
            let read = cx
                .background_spawn(async move {
                    parts.into_iter().map(|(ix, from, to, load)| (ix, load(from, to))).collect::<Vec<_>>()
                })
                .await;
            let mut items = items;
            // From the end, so the places before stay put. A part that could not be read stays a
            // placeholder line.
            for (ix, part) in read.into_iter().rev() {
                if let Ok(part) = part {
                    items.splice(ix..ix + 1, part);
                }
            }
            let text = to_markdown(&items, name);
            let _ = cx.update(|window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                gpui_kit::component::WindowExt::push_notification(window, "Conversation copied as Markdown", cx);
            });
        })
        .detach();
    }

    /// The divider where the context was compacted, its summary on request.
    /// The person's message, its long pastes folded to a line that opens them.
    fn render_user_text(&self, key: &str, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let parts = paste::parts(text);
        if !parts.iter().any(|p| matches!(p, paste::Part::Pasted(_))) {
            return text.to_string().into_any_element();
        }
        let theme = cx.theme();
        let (muted, border, hover, code_bg) = (theme.muted_foreground, theme.border, theme.secondary_hover, theme.background);
        let mut col = div().flex().flex_col().gap_1();
        for (k, part) in parts.into_iter().enumerate() {
            match part {
                paste::Part::Text(t) => {
                    if !t.trim().is_empty() {
                        col = col.child(t.trim().to_string());
                    }
                }
                paste::Part::Pasted(p) => {
                    let id = format!("paste:{key}:{k}");
                    let open = self.toggled.contains(&id);
                    let toggle = id.clone();
                    col = col.child(
                        div()
                            .rounded(px(8.))
                            .border_1()
                            .border_color(border)
                            .overflow_hidden()
                            .child(
                                div()
                                    .id(SharedString::from(id))
                                    .px_2()
                                    .py_1()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_xs()
                                    .text_color(muted)
                                    .cursor_pointer()
                                    .hover(|d| d.bg(hover))
                                    .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall())
                                    .child(format!("Pasted text · {}", paste::size(p)))
                                    .on_click(cx.listener(move |panel, _, _, cx| {
                                        if !panel.toggled.remove(&toggle) {
                                            panel.toggled.insert(toggle.clone());
                                        }
                                        panel.list.remeasure();
                                        cx.notify();
                                    })),
                            )
                            .when(open, |d| {
                                d.child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .bg(code_bg)
                                        .font_family("monospace")
                                        .text_xs()
                                        .flex()
                                        .flex_col()
                                        .children(p.lines().map(|l| if l.is_empty() { " ".to_string() } else { l.to_string() })),
                                )
                            }),
                    );
                }
            }
        }
        col.into_any_element()
    }

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

    /// Another agent's message (a subagent's report): one row with its first line, the whole
    /// report on request.
    fn render_agent_message(&self, from: &str, title: &str, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, hover, violet) =
            (theme.foreground, theme.muted_foreground, theme.border, theme.secondary_hover, theme.magenta);
        let key = format!("report:{from}:{}", text.len());
        let open = self.toggled.contains(&key);
        let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
        let k = key.clone();
        let header = div()
            .id(SharedString::from(key.clone()))
            .h(px(22.))
            .px_1()
            .flex()
            .items_center()
            .gap_1()
            .text_xs()
            .cursor_pointer()
            .hover(|d| d.bg(hover))
            .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall().text_color(muted))
            .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(violet).child("report"))
            .child(div().flex_none().max_w(relative(0.4)).truncate().text_color(fg).child(title.to_string()))
            .when(!open, |d| {
                d.child(div().flex_1().min_w_0().truncate().text_color(muted).child(one_line(&first.replace("**", ""), 200)))
            })
            .on_click(cx.listener(move |p, _, _, cx| {
                if !p.toggled.remove(&k) {
                    p.toggled.insert(k.clone());
                }
                p.list.remeasure();
                cx.notify();
            }));
        div()
            .flex()
            .flex_col()
            .border_1()
            .border_color(border)
            .rounded_md()
            .bg(violet.opacity(0.05))
            .overflow_hidden()
            .child(header)
            .when(open, |d| {
                d.child(
                    div()
                        .px_2()
                        .py_1()
                        .border_t_1()
                        .border_color(border)
                        .text_color(fg)
                        .child(self.markdown(SharedString::from(format!("{key}-body")), text, cx)),
                )
            })
            .into_any_element()
    }

    /// One item of the conversation, for the list.
    fn render_entry(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let item = self.item_at(ix, cx);
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
                    .children(match self.preview_text(ix, &item, cx) {
                        Some(text) => vec![self.render_preview(text, cx)],
                        None => self.render_items(std::slice::from_ref(&item), &format!("main{ix}"), cx),
                    }),
            )
            .into_any_element()
    }

    /// The text of item `ix` when it is a preview of the reply being written (read off the
    /// agent's screen), not text from its record.
    fn preview_text(&self, ix: usize, item: &Item, cx: &App) -> Option<String> {
        if self.agent.is_some() || self.session.read(cx).chat.preview_item() != Some(ix) {
            return None;
        }
        match item {
            Item::Assistant { text, .. } => Some(text.clone()),
            _ => None,
        }
    }

    /// The reply being written, as plain text with its lines kept (the screen has already
    /// turned its Markdown into terminal text); the finished reply replaces it, formatted.
    fn render_preview(&self, text: String, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted) = (theme.foreground, theme.muted_foreground);
        let mut col = div().w_full().flex().flex_col().text_color(fg).line_height(rems(1.5));
        for line in text.lines() {
            col = col.child(if line.trim().is_empty() { div().h(rems(0.75)) } else { div().child(line.to_string()) });
        }
        col.child(
            div()
                .pt_1()
                .flex()
                .items_center()
                .gap_1()
                .text_xs()
                .text_color(muted)
                .child(self.spinner(muted).xsmall())
                .child("Writing…"),
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
            // Code blocks: the language and a copy button; a one-line `!` command also gets Run,
            // which runs it (without the `!`) in a terminal tab of the session's folder.
            .code_block_actions({
                let panel = entity.clone();
                move |block, _, cx| {
                let code = block.code();
                let lang = block.lang().unwrap_or_else(|| "code".into());
                let muted = cx.theme().muted_foreground;
                let theme = cx.theme();
                let (go, go_hover, go_active) = (theme.success, theme.success_hover, theme.success_active);
                // The dark theme's green is light: dark text on it reads, its own green does not.
                let go_fg = if theme.is_dark() { theme.background } else { theme.success_foreground };
                let command = shell_command(&code);
                let panel = panel.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(11.))
                    .text_color(muted)
                    .child(lang)
                    .when_some(command, |d, command| {
                        // A green pill, so it reads as a button at a glance: brighter under the
                        // pointer, darker while pressed.
                        d.child(
                            div()
                                .id("run-code")
                                .h(px(20.))
                                .px(px(8.))
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .rounded(px(10.))
                                .bg(go)
                                .text_color(go_fg)
                                .font_weight(FontWeight::SEMIBOLD)
                                .shadow_xs()
                                .cursor_pointer()
                                .hover(|d| d.bg(go_hover).shadow_sm())
                                .active(|d| d.bg(go_active))
                                .child(Icon::new(gpui_kit::assets::IconName::Play).xsmall().text_color(go_fg))
                                .child("Run")
                                .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Run it in a terminal in this folder").build(w, cx))
                                .on_click(move |_, _, cx| {
                                    let text = command.clone();
                                    let _ = panel.update(cx, |_, cx| cx.emit(ChatPanelEvent::RunCommand(text)));
                                }),
                        )
                    })
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
                }
            })
            .on_link_click(move |url, ev, _, cx| {
                if let Some((path, line)) = links::parse_url(url) {
                    let _ = entity.update(cx, |p, cx| p.open(path, line, cx));
                } else if keleuo_core::web::is_web_link(url) && !ev.modifiers().platform {
                    // A web page opens in the Web panel; Cmd+click hands it to the browser.
                    let url = url.to_string();
                    let _ = entity.update(cx, |_, cx| cx.emit(ChatPanelEvent::OpenWeb(url)));
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
        let (green, red, page_bg) = (theme.green, theme.red, theme.background);
        // Diffs start open; other cards start closed.
        let default_open = tool.diff.is_some();
        let open = default_open != self.toggled.contains(&tool.id);
        let id = tool.id.clone();
        let status = if tool.running() {
            self.spinner(muted).xsmall().into_any_element()
        } else if !tool.done() {
            // Interrupted, or replayed without its result.
            Icon::new(IconName::Minus).xsmall().text_color(muted).into_any_element()
        } else if tool.is_error {
            Icon::new(IconName::CircleX).xsmall().text_color(red).into_any_element()
        } else {
            Icon::new(IconName::Check).xsmall().text_color(green).into_any_element()
        };
        // Live while it runs; afterwards how long it took, when that is worth saying.
        let elapsed = tool
            .elapsed()
            .filter(|d| tool.running() || d.as_secs() >= 1)
            .map(format_elapsed);
        let stats = tool.diff.as_ref().map(|d| {
            let path = d.path.clone();
            div()
                .flex()
                .gap_1()
                .flex_none()
                .child(div().text_color(green).child(format!("+{}", d.added())))
                .child(div().text_color(red).child(format!("-{}", d.removed())))
                // The file as a diff of everything the session changed in it.
                .when(tool.done() && !tool.is_error, |d| {
                    d.child(
                        div()
                            .id(SharedString::from(format!("open-diff-{}", tool.id)))
                            .px_1()
                            .rounded_sm()
                            .text_color(muted)
                            .hover(|d| d.bg(hover.opacity(0.6)).text_color(fg))
                            .child("Open diff")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(ChatPanelEvent::OpenDiff(path.clone()));
                            })),
                    )
                })
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
            .when_some(elapsed, |d, e| d.child(div().flex_none().text_color(muted).child(e)))
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
        // A known needless failure: what happened, and a way to stop it happening again.
        if let Some(issue) = &tool.issue {
            card = card.child(self.render_issue_notice(&tool.id, issue, cx));
        }
        // While it runs, its latest output, newest line at the bottom, whether the card is
        // open or not.
        if tool.running()
            && let Some(live) = tool.live.as_deref().filter(|l| !l.trim().is_empty())
        {
            let lines: Vec<&str> = live.trim_end_matches('\n').lines().collect();
            let tail = lines[lines.len().saturating_sub(LIVE_LINES)..].join("\n");
            card = card.child(
                div()
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(border)
                    .bg(page_bg.opacity(0.6))
                    .font_family(MONO)
                    .text_size(px(11.))
                    .line_height(px(15.))
                    .text_color(muted)
                    .max_h(px(15. * LIVE_LINES as f32 + 8.))
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .justify_end()
                    .child(tail),
            );
        }
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
                    let copy = paste::without_markers(text);
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
                                .child(self.render_user_text(&format!("{key}-{ix}"), text, cx)),
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
                Item::AgentMessage { from, title, text, .. } => self.render_agent_message(from, title, text, cx),
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
            AgentStatus::Running => self.spinner(violet).xsmall().into_any_element(),
            AgentStatus::Done => Icon::new(IconName::Check).xsmall().text_color(green).into_any_element(),
            AgentStatus::Failed => Icon::new(IconName::CircleX).xsmall().text_color(red).into_any_element(),
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

    /// A watched subagent: its description and state, then its items as they arrive. No
    /// message box: it is the subagent's work, not a conversation to write into.
    fn render_agent_view(&mut self, id: &str, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (muted, border, bg) = (theme.muted_foreground, theme.border, theme.background);
        let (title, state) = match self.session.read(cx).chat.agent(id) {
            Some(a) => {
                let status = match a.status {
                    AgentStatus::Running => "running",
                    AgentStatus::Done => "done",
                    AgentStatus::Failed => "failed",
                };
                let mut parts = vec![a.kind.clone(), status.to_string()];
                if let Some(tool) = &a.current_tool {
                    parts.push(format!("using {tool}"));
                }
                if let Some(tokens) = a.total_tokens {
                    parts.push(format!("{tokens} tokens"));
                }
                (a.description.clone(), parts.join(" · "))
            }
            None => ("Subagent".to_string(), "no longer in this conversation".to_string()),
        };
        let len = self.item_count(cx);
        if self.list_len != len {
            self.sync_list(len);
        }
        let empty = self.list_len == 0;
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(bg)
            .track_focus(&self.focus)
            .on_action(cx.listener(|p, _: &CopyConversation, window, cx| p.copy_conversation(window, cx)))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .child(div().text_sm().truncate().child(title))
                    .child(div().text_xs().text_color(muted).truncate().child(state)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when(empty, |d| d.p_3().text_xs().text_color(muted).child("Nothing from it yet."))
                    .when(!empty, |d| {
                        d.child(
                            list(self.list.clone(), cx.processor(|this, ix, _window, cx| this.render_entry(ix, cx)))
                                .size_full(),
                        )
                    }),
            )
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
        let weak = cx.entity().downgrade();
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
            .child(
                Button::new("chat-options")
                    .icon(IconName::Ellipsis)
                    .xsmall()
                    .ghost()
                    .tab_stop(false)
                    .dropdown_menu(move |menu, _, _| {
                        let weak = weak.clone();
                        menu.item(
                            gpui_kit::component::menu::PopupMenuItem::new("Copy conversation (Cmd+Alt+C)").on_click(
                                move |_, window, cx| {
                                    let _ = weak.update(cx, |p, cx| p.copy_conversation(window, cx));
                                },
                            ),
                        )
                    }),
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
        // Some room below the tabs, so they don't sit on the window's bottom edge.
        Some(
            div().flex_none().bg(ground).pb(px(VIEW_STRIP_GAP_BELOW)).child(
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
                ),
            )
            .into_any_element(),
        )
    }

    /// The interface waits on a question before it is up: Trust / Don't trust for the folder
    /// question, else a pointer to the terminal.
    /// While the agent thinks, before its first text or tool call: a shimmering line above the
    /// composer, so a quiet stretch reads as work in progress rather than a stall.
    fn render_thinking(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.session.read(cx);
        // A streaming text item means the answer has started (its first delta sets the status
        // to "writing"; the item appears a moment earlier).
        let answering = matches!(session.chat.items.last(), Some(Item::Assistant { streaming: true, .. }));
        // Shown for the whole turn while no answer is streaming: a session run through its
        // terminal reports "working" or the tool it runs, never "thinking".
        if !session.chat.busy || answering || session.chat.needs_input() || session.view == View::Terminal {
            return None;
        }
        let name = session.backend.name();
        let label = match session.chat.status.as_deref() {
            None | Some("thinking" | "working" | "sending" | "requesting") => format!("{name} is thinking…"),
            Some(_) => format!("{name} is working…"),
        };
        let theme = cx.theme();
        let (ring, border, accent, muted, theme_fg) = (theme.ring, theme.border, theme.accent, theme.muted_foreground, theme.foreground);
        let elapsed = self.busy_since.map(|t| format_elapsed(t.elapsed()));
        Some(
            div()
                .flex_none()
                .px_2()
                .py_1()
                .flex()
                .items_center()
                .gap_2()
                .border_t_1()
                .border_color(border)
                .bg(accent)
                .text_sm()
                .child(self.spinner(ring).small())
                .child(div().text_color(theme_fg).child(label))
                // How long this turn has been going, at the right.
                .child(div().flex_1())
                .when_some(elapsed, |d, e| d.child(div().flex_none().text_xs().text_color(muted).child(e)))
                .into_any_element(),
        )
    }

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

    /// In the terminal view, the files the agent changed this session, as links to their diffs.
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
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(ChatPanelEvent::OpenDiff(path.clone())))),
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

/// Shift+Enter on a list line in the message box: the next item's marker on the new line, or
/// the marker taken away on an item left empty (ending the list). False when it is a plain new
/// line. Edits only around the cursor, so paste tokens stay tokens and Undo takes it back.
fn continue_list(input: &Entity<TextareaState>, window: &mut Window, cx: &mut App) -> bool {
    let (text, selected) = {
        let input = input.read(cx);
        (input.value(), input.selected_range())
    };
    if !selected.is_empty() {
        return false;
    }
    let cursor = selected.start;
    let start = text[..cursor].rfind('\n').map_or(0, |i| i + 1);
    let end = text[cursor..].find('\n').map_or(text.len(), |i| cursor + i);
    let Some(step) = compose::list_break(&text[start..end], cursor - start) else { return false };
    // Splitting an item: the spaces after the cursor go, so the rest follows the marker as typed.
    let spaces = text[cursor..end].len() - text[cursor..end].trim_start_matches([' ', '\t']).len();
    input.update(cx, |s, cx| match step {
        ListBreak::Continue(marker) => {
            if spaces > 0 {
                s.set_selected_range(cursor..cursor + spaces, cx);
            }
            s.replace(format!("\n{marker}"), window, cx)
        }
        ListBreak::End => {
            s.set_selected_range(start..end, cx);
            s.replace("", window, cx);
        }
    });
    true
}

/// A paste's token in the message box: a small raised pill (rounded, lit from above, a soft
/// shadow) with its size.
fn paste_pill(token: &InlineTokenContext, _: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let base = theme.secondary;
    let (top, bottom, edge) = if theme.is_dark() {
        (Hsla { l: (base.l + 0.13).min(1.), ..base }, Hsla { l: (base.l + 0.04).min(1.), ..base }, theme.foreground.opacity(0.18))
    } else {
        (Hsla { l: (base.l + 0.03).min(1.), ..base }, Hsla { l: (base.l - 0.05).max(0.), ..base }, theme.border)
    };
    let edge = if token.is_selected() { theme.ring } else { edge };
    div()
        .h(token.line_height() - px(2.))
        .mx(px(1.))
        .px(px(7.))
        .flex()
        .items_center()
        .gap(px(4.))
        .rounded(px(7.))
        .border_1()
        .border_color(edge)
        .bg(linear_gradient(180., linear_color_stop(top, 0.), linear_color_stop(bottom, 1.)))
        .shadow_xs()
        .text_xs()
        .text_color(theme.foreground)
        .child(Icon::new(gpui_kit::assets::IconName::FileText).xsmall().text_color(theme.muted_foreground))
        .child(token.token().label().clone())
        .into_any_element()
}

impl Render for ChatPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(id) = self.agent.clone() {
            return self.render_agent_view(&id, cx);
        }
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
        let len = self.item_count(cx);
        if self.list_len != len {
            self.sync_list(len);
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
            .on_action(cx.listener(|p, _: &CopyConversation, window, cx| p.copy_conversation(window, cx)))
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
            .children(self.render_thinking(cx))
            .child({
                let slash = self.render_slash(cx);
                self.render_composer(slash, window, cx)
            }))
            .children(self.render_views(cx))
            .children(self.render_fix(cx))
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
    /// Tests: send `text` with what is attached and pasted.
    pub fn test_send(&mut self, text: &str, cx: &mut Context<Self>) {
        let images = std::mem::take(&mut self.attachments);
        let text = paste::expand(text, &std::mem::take(&mut self.pastes), true);
        self.session.update(cx, |s, cx| s.send_with(&text, images, cx));
    }

    /// Tests: paste `text` into the message box (short text goes in as typed).
    pub fn test_paste(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if paste::is_long(&text) {
            self.paste_long(text, window, cx);
        } else {
            self.input.update(cx, |s, cx| s.insert(text, window, cx));
        }
    }

    /// The `/` list for what is typed: its matches, when the message is a lone `/word` and the
    /// list was not closed. Reads the skills the first time.
    fn slash_matches(&mut self, cx: &mut Context<Self>) -> Vec<keleuo_core::slash::Choice> {
        let value = self.input.read(cx).value().to_string();
        let Some(query) = keleuo_core::slash::typed(&value).map(str::to_string) else {
            self.slash_closed = false;
            return Vec::new();
        };
        if query != self.slash_query {
            self.slash_query = query.clone();
            self.slash_ix = 0;
            self.slash_closed = false;
        }
        let Some(choices) = self.slash_choices.clone() else {
            self.load_slash_choices(cx);
            return Vec::new();
        };
        if self.slash_closed {
            return Vec::new();
        }
        keleuo_core::slash::matching(&choices, &query, 8).into_iter().cloned().collect()
    }

    fn load_slash_choices(&mut self, cx: &mut Context<Self>) {
        self.slash_choices = Some(Rc::new(Vec::new()));
        let s = self.session.read(cx);
        let (backend, host, root) = (s.backend.clone(), s.host(), s.cwd.clone());
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async move {
                    let catalog = backend.skills(host.as_ref(), &root);
                    keleuo_core::slash::choices(&catalog, backend.builtin_commands(), backend.name())
                })
                .await;
            let _ = this.update(cx, |p, cx| {
                p.slash_choices = Some(Rc::new(found));
                cx.notify();
            });
        })
        .detach();
    }

    /// Put the highlighted `/` choice in the message box, replacing what was typed after the `/`.
    fn accept_slash(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let typed = 1 + self.slash_query.len();
        let text = format!("/{name} ");
        self.input.update(cx, |s, cx| {
            s.set_selected_range(0..typed, cx);
            s.replace(text, window, cx);
        });
        self.slash_closed = true;
        cx.notify();
    }

    /// Keys while the `/` list is up: Up and Down move, Tab (or Enter on a name not yet typed in
    /// full) takes the highlighted one, Escape closes the list. True when the key was used.
    fn slash_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let found = self.slash_matches(cx);
        if found.is_empty() {
            return false;
        }
        match key {
            "up" => self.slash_ix = self.slash_ix.checked_sub(1).unwrap_or(found.len() - 1),
            "down" => self.slash_ix = (self.slash_ix + 1) % found.len(),
            "escape" => self.slash_closed = true,
            "tab" | "enter" => {
                let pick = &found[self.slash_ix.min(found.len() - 1)];
                // Typed in full already: Enter sends it.
                if key == "enter" && pick.name == self.slash_query {
                    return false;
                }
                let name = pick.name.clone();
                self.accept_slash(&name, window, cx);
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    /// The `/` list over the message box.
    fn render_slash(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let found = self.slash_matches(cx);
        if found.is_empty() {
            return None;
        }
        let ix = self.slash_ix.min(found.len() - 1);
        let theme = cx.theme();
        let (fg, muted, sel, border, bg) = (theme.foreground, theme.muted_foreground, theme.secondary, theme.border, theme.popover);
        let mut list = div()
            .id("slash-list")
            .w_full()
            .mb_1()
            .p_1()
            .rounded(px(12.))
            .border_1()
            .border_color(border)
            .bg(bg)
            .shadow_md()
            .flex()
            .flex_col();
        for (n, choice) in found.into_iter().enumerate() {
            let name = choice.name.clone();
            list = list.child(
                div()
                    .id(("slash", n))
                    .px_2()
                    .py(px(5.))
                    .rounded(px(8.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .cursor_pointer()
                    .when(n == ix, |d| d.bg(sel))
                    .hover(|d| d.bg(sel))
                    .child(div().flex_none().text_sm().font_family("monospace").text_color(fg).child(format!("/{}", choice.name)))
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(muted).child(choice.description))
                    .child(div().flex_none().text_xs().text_color(muted).child(choice.from))
                    .on_click(cx.listener(move |p, _, window, cx| {
                        p.accept_slash(&name, window, cx);
                        p.input.update(cx, |s, cx| s.focus(window, cx));
                    })),
            );
        }
        Some(list.into_any_element())
    }

    /// "Other model…": a field for any model name the backend takes; Enter switches to it and
    /// keeps it among the recent ones, Escape or clicking away drops it.
    fn start_model_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Model name, e.g. claude-opus-4-1"));
        input.update(cx, |s, cx| s.focus(window, cx));
        let sub = cx.subscribe_in(&input, window, |p, input, ev: &InputEvent, _, cx| match ev {
            InputEvent::PressEnter { .. } => {
                let name = input.read(cx).value().trim().to_string();
                p.model_input = None;
                if !name.is_empty() {
                    remember_model(&name, cx);
                    p.session.update(cx, |s, cx| s.set_model(&name, cx));
                }
                cx.notify();
            }
            InputEvent::Blur => {
                p.model_input = None;
                cx.notify();
            }
            _ => {}
        });
        self.model_input = Some((input, sub));
        cx.notify();
    }

    /// Tests: "Other model…" with `name` typed and Enter.
    pub fn test_other_model(&mut self, name: &str, cx: &mut Context<Self>) {
        remember_model(name, cx);
        self.session.update(cx, |s, cx| s.set_model(name, cx));
    }

    /// Tests: a paste's "Show in text field" (`inline`) or "Remove".
    pub fn test_unfold(&mut self, n: usize, inline: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.unfold_paste(n, inline, window, cx);
    }

    /// Tests: press send.
    pub fn test_send_composed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.send_composed(window, cx);
    }

    /// Send what the message box holds: its text with the long pastes put back (marked, so
    /// the chat folds them), and the attached images. The box empties.
    fn send_composed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.input.read(cx).value().to_string();
        if typed.trim().is_empty() && self.attachments.is_empty() {
            return;
        }
        let text = paste::expand(&typed, &std::mem::take(&mut self.pastes), true);
        self.input.update(cx, |s, cx| s.set_value("", window, cx));
        // Sending goes back to the end and follows it.
        self.list.set_follow_mode(FollowMode::Tail);
        let images = std::mem::take(&mut self.attachments);
        self.session.update(cx, |s, cx| s.send_with(&text, images, cx));
    }

    /// A long paste: kept aside, and a token for it put at the cursor. The token is one unit
    /// in the box (the cursor steps over it, Backspace takes it whole), drawn as a pill; its
    /// text is the `[Pasted text #n · …]` that sending expands.
    fn paste_long(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.pastes.len() + 1;
        let typed = paste::token(n, &text);
        let token = InlineToken::new(format!("paste-{n}"), typed.clone()).with_label(format!("Pasted text · {}", paste::size(&text)));
        self.pastes.push(text);
        self.input.update(cx, |s, cx| {
            if s.replace_with_token(token, window, cx).is_err() {
                s.insert(typed, window, cx);
            }
        });
        cx.notify();
    }

    /// Paste `n` back into the message box as plain text (`inline`), or out of it. The other
    /// tokens stay tokens.
    fn unfold_paste(&mut self, n: usize, inline: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pasted) = self.pastes.get(n - 1).cloned() else { return };
        let id = format!("paste-{n}");
        let content = self.input.read(cx).content();
        let text = content.text().to_string();
        let (mut out, mut kept, mut last) = (String::new(), Vec::new(), 0);
        for span in content.tokens() {
            let r = span.range();
            out.push_str(&text[last..r.start]);
            last = r.end;
            if span.token().id().as_ref() == id {
                if inline {
                    out.push_str(&pasted);
                }
            } else {
                let start = out.len();
                out.push_str(&text[r]);
                kept.push((start..out.len(), span.token().clone()));
            }
        }
        out.push_str(&text[last..]);
        let mut rebuilt = InputContent::new(out.clone());
        for (range, token) in kept {
            rebuilt = match rebuilt.clone().with_token(range, token) {
                Ok(c) => c,
                Err(_) => rebuilt,
            };
        }
        self.input.update(cx, |s, cx| s.set_value(rebuilt, window, cx));
        cx.notify();
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
        if let Some(dir) = keleuo_core::images::session_dir(id.as_deref())
            && let Ok(path) = keleuo_core::images::store(&dir, bytes, ext)
        {
            self.attach([path], cx);
        }
    }

    /// The composer: a rounded box floating at the bottom of the column, with the images to
    /// send, the text, and the session's controls (attach, model, permission mode, send/stop).
    fn render_composer(&self, slash: Option<AnyElement>, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (border, muted, fg, field, accent) = (theme.border, theme.muted_foreground, theme.foreground, theme.secondary, theme.ring);
        let session = self.session.read(cx);
        let busy = session.chat.busy;
        let caps = session.backend.capabilities();
        let model = session.chat.model.clone().or_else(|| session.model.clone()).unwrap_or_else(|| "default model".into());
        let mode = session.live_mode.clone().or_else(|| session.permission_mode.clone()).unwrap_or_else(|| "default".into());
        let has_text = !self.input.read(cx).value().trim().is_empty() || !self.attachments.is_empty();
        let restarting = session.restarting.clone();
        let queued = session.queued_count();
        let name = session.backend.name();
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

        // Long pastes still in the box: what each holds, to put back in the text or drop.
        let value = self.input.read(cx).value().to_string();
        let mut paste_chips = 0;
        for n in paste::tokens_in(&value) {
            let Some(text) = self.pastes.get(n.wrapping_sub(1)) else { continue };
            paste_chips += 1;
            let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
            let action = |id: &'static str, label: &'static str, inline: bool| {
                div()
                    .id((id, n))
                    .px_1()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|d| d.text_color(fg))
                    .child(label)
                    .on_click(cx.listener(move |p, _, window, cx| p.unfold_paste(n, inline, window, cx)))
            };
            chips = chips.child(
                div()
                    .h(px(56.))
                    .max_w(px(280.))
                    .px_2()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(border)
                    .bg(field)
                    .flex()
                    .flex_col()
                    .justify_center()
                    .text_xs()
                    .text_color(muted)
                    .child(div().text_color(fg).truncate().child(format!("Pasted text · {}", paste::size(text))))
                    .child(div().truncate().font_family("monospace").child(first))
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(action("paste-inline", "Show in text field", true))
                            .child(action("paste-drop", "Remove", false)),
                    ),
            );
        }

        // The backend's own names (aliases, each the latest of its family), the names typed in
        // before, and "Other model…" to type one. The running one is ticked; an alias shows
        // what it stands for.
        let models: Vec<String> = caps.models.clone();
        let recent: Vec<String> = cx.global::<crate::AppSettings>().0.borrow().recent_models.clone();
        let (w_model, w_mode) = (weak.clone(), weak.clone());
        let panel_for_menu = entity.clone();
        // What was picked (an alias or a typed name), and what it runs as.
        let (picked, running_as) = (session.model.clone(), session.chat.model.clone());
        let model_button = Button::new("model")
            .xsmall()
            .ghost()
            .label(short_model(&model))
            .tooltip("Model")
            .dropdown_menu(move |menu, _, _| {
                use gpui_kit::component::menu::PopupMenuItem;
                let mut menu = menu;
                let is = |m: &str| match (&picked, &running_as) {
                    (Some(p), _) => p == m,
                    (None, Some(r)) => r.contains(m),
                    (None, None) => false,
                };
                for m in &models {
                    let (w, m2) = (w_model.clone(), m.clone());
                    let running = is(m);
                    let label = match (&running_as, running) {
                        (Some(r), true) if r != m => format!("{} ({})", capitalize(m), r),
                        _ => capitalize(m),
                    };
                    menu = menu.item(PopupMenuItem::new(label).checked(running).on_click(move |_, _, cx| {
                        let _ = w.update(cx, |s, cx| s.set_model(&m2, cx));
                    }));
                }
                if !recent.is_empty() {
                    menu = menu.separator();
                    for m in &recent {
                        let (w, m2) = (w_model.clone(), m.clone());
                        menu = menu.item(PopupMenuItem::new(m.clone()).checked(picked.as_deref() == Some(m.as_str())).on_click(move |_, _, cx| {
                            remember_model(&m2, cx);
                            let _ = w.update(cx, |s, cx| s.set_model(&m2, cx));
                        }));
                    }
                }
                let panel = panel_for_menu.clone();
                menu.separator().item(PopupMenuItem::new("Other model…").on_click(move |_, window, cx| {
                    let _ = panel.update(cx, |p, cx| p.start_model_input(window, cx));
                }))
            });
        // Typing a model name: the field takes the button's place.
        let model_control = match &self.model_input {
            Some((input, _)) => div()
                .w(px(240.))
                .capture_key_down(cx.listener(|p, ev: &KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        cx.stop_propagation();
                        p.model_input = None;
                        cx.notify();
                    }
                }))
                .child(Input::new(input).xsmall())
                .into_any_element(),
            None => model_button.into_any_element(),
        };
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
                p.send_composed(window, cx);
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
            .flex_col()
            .items_center()
            .px_4()
            .pb_3()
            .pt_1()
            // The `/` list, just above the box it completes.
            .children(slash.map(|s| div().w_full().max_w(px(COLUMN)).child(s)))
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
                    // Shift+Enter on a list item: before the box puts in its plain new line.
                    // Enter with the `/` list up takes the highlighted name.
                    .capture_action(cx.listener(|p, action: &Enter, window, cx| {
                        let used = !action.secondary
                            && if action.shift { continue_list(&p.input, window, cx) } else { p.slash_key("enter", window, cx) };
                        if used {
                            cx.stop_propagation();
                        }
                    }))
                    // Up, Down, Tab and Escape steer the `/` list while it is up (they are the
                    // box's own actions, so they are caught before it moves its cursor).
                    .capture_action(cx.listener(|p, _: &MoveUp, window, cx| {
                        if p.slash_key("up", window, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .capture_action(cx.listener(|p, _: &MoveDown, window, cx| {
                        if p.slash_key("down", window, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .capture_action(cx.listener(|p, _: &IndentInline, window, cx| {
                        if p.slash_key("tab", window, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .capture_action(cx.listener(|p, _: &Escape, window, cx| {
                        if p.slash_key("escape", window, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .when_some(restarting, |d, why| {
                        // Restarting (a new model or mode): what is typed now waits for it.
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_size(px(11.5))
                                .text_color(muted)
                                .child(Icon::new(IconName::LoaderCircle).xsmall())
                                .child(match queued {
                                    0 => format!("Restarting {name} {why}. A message sent now goes when it is up."),
                                    n => format!("Restarting {name} {why}. {n} message{} waiting to go.", if n == 1 { "" } else { "s" }),
                                }),
                        )
                    })
                    .when(!self.attachments.is_empty() || paste_chips > 0, |d| d.child(chips))
                    .child(
                        Textarea::new(&self.input)
                            .appearance(false)
                            .text_sm()
                            .token(paste_pill)
                            .on_paste(move |item, window, cx| {
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
                                // Long text: a token in the box, the text kept aside (the box is
                                // being changed now, so after this).
                                if !took
                                    && let Some(text) = item.text().filter(|t| paste::is_long(t))
                                {
                                    let target = paste_target.clone();
                                    window.defer(cx, move |window, cx| {
                                        let _ = target.update(cx, |p, cx| p.paste_long(text, window, cx));
                                    });
                                    took = true;
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
                            .child(model_control)
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
/// Keep a typed model name among the recent ones, saved.
fn remember_model(name: &str, cx: &mut App) {
    let settings = cx.global::<crate::AppSettings>().0.clone();
    settings.borrow_mut().remember_model(name);
    let _ = settings.borrow().save();
}

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

/// Something with a live clock runs: a subagent or a tool call.
fn clocks_running(chat: &keleuo_core::conversation::Conversation) -> bool {
    chat.busy || chat.running_agents().next().is_some() || chat.running_tools().next().is_some()
}

/// `42s`, `3m 05s`, `2h 05m 09s`, `1d 3h 05m 09s`.
fn format_elapsed(d: std::time::Duration) -> String {
    let s = d.as_secs();
    let (days, h, m, sec) = (s / 86_400, s / 3600 % 24, s / 60 % 60, s % 60);
    match (days, h, m) {
        (0, 0, 0) => format!("{sec}s"),
        (0, 0, _) => format!("{m}m {sec:02}s"),
        (0, _, _) => format!("{h}h {m:02}m {sec:02}s"),
        _ => format!("{days}d {h}h {m:02}m {sec:02}s"),
    }
}

#[cfg(test)]
mod elapsed_tests {
    use super::format_elapsed;
    use std::time::Duration;

    #[test]
    fn reads_from_seconds_to_days() {
        let t = |s| format_elapsed(Duration::from_secs(s));
        assert_eq!(t(42), "42s");
        assert_eq!(t(185), "3m 05s");
        assert_eq!(t(2 * 3600 + 5 * 60 + 9), "2h 05m 09s");
        assert_eq!(t(86_400 + 3 * 3600 + 5 * 60 + 9), "1d 3h 05m 09s");
    }
}

/// A code block that is one shell command written as for the agent's shell (`! git push`): the
/// command, without the `!`.
fn shell_command(code: &str) -> Option<String> {
    let mut lines = code.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next()?;
    let command = first.strip_prefix('!')?.trim();
    (lines.next().is_none() && !command.is_empty()).then(|| command.to_string())
}

#[cfg(test)]
mod shell_command_tests {
    use super::shell_command;

    #[test]
    fn one_bang_line_is_a_command() {
        assert_eq!(shell_command("! git push -u origin main\n").as_deref(), Some("git push -u origin main"));
        assert_eq!(shell_command("!ls").as_deref(), Some("ls"));
        assert_eq!(shell_command("ls -la"), None);
        assert_eq!(shell_command("! a\n! b"), None, "several commands: copy them instead");
        assert_eq!(shell_command("!"), None);
    }
}

#[cfg(test)]
mod list_tests {
    use gpui_kit::component::input::{Enter, Textarea, TextareaState};
    use gpui_kit::{AppContext, Context, Entity, Focusable, IntoElement, Render, TestAppContext, Window, div, prelude::*};

    use super::continue_list;

    /// A message box wired as the composer is: Shift+Enter caught before the box's own new line.
    struct Composer(Entity<TextareaState>);
    impl Render for Composer {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .capture_action(cx.listener(|p, action: &Enter, window, cx| {
                    if action.shift && !action.secondary && continue_list(&p.0, window, cx) {
                        cx.stop_propagation();
                    }
                }))
                .child(Textarea::new(&self.0))
        }
    }

    #[gpui_kit::test]
    fn shift_enter_continues_and_ends_a_list(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let mut input = None;
        let handle = cx
            .update(|cx| {
                gpui_kit::open_window(Default::default(), cx, |window, cx| {
                    let state = cx.new(|cx| TextareaState::new(window, cx).submit_on_enter(true));
                    window.focus(&state.focus_handle(cx), cx);
                    input = Some(state.clone());
                    cx.new(|_| Composer(state))
                })
                .unwrap()
                .0
            });
        let input = input.unwrap();
        let value = |cx: &mut TestAppContext| input.read_with(cx, |s, _| s.value().to_string());
        cx.update_window(handle, |_, window, cx| {
            input.update(cx, |s, cx| s.set_value("1. milk", window, cx));
            input.update(cx, |s, cx| s.set_selected_range(7..7, cx));
        })
        .unwrap();
        cx.simulate_keystrokes(handle, "shift-enter");
        assert_eq!(value(cx), "1. milk\n2. ");
        cx.simulate_keystrokes(handle, "shift-enter");
        assert_eq!(value(cx), "1. milk\n");
        cx.simulate_keystrokes(handle, "shift-enter");
        assert_eq!(value(cx), "1. milk\n\n", "after the list, a plain new line");
        // In the middle of an item: the rest goes to the next item.
        cx.update_window(handle, |_, window, cx| {
            input.update(cx, |s, cx| s.set_value("- eggs and ham", window, cx));
            input.update(cx, |s, cx| s.set_selected_range(6..6, cx));
        })
        .unwrap();
        cx.simulate_keystrokes(handle, "shift-enter");
        assert_eq!(value(cx), "- eggs\n- and ham");
        cx.simulate_keystrokes(handle, "cmd-z");
        assert_eq!(value(cx), "- eggs and ham", "Undo takes the new item back");
    }
}
