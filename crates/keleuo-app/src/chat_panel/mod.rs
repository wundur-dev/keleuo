//! The chat panel: the conversation rendered natively from the agent's events. Assistant text as markdown,
//! tool calls as collapsible cards, file edits as diffs, permission prompts with Allow and
//! Deny, and a one-line status in the header. Every file reference is a link.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::input::{Enter, Escape, IndentInline, MoveDown, MoveUp, InlineToken, TextDecoration, TextDecorationCollection, TextareaMode, InlineTokenContext, Input, InputContent, InputEvent, InputState, Textarea, TextareaState};

use crate::agent_session::{Asking, Fill, Spend, View};
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
use keleuo_core::agent::{Issue, ModelOption};
use keleuo_core::diff::{FileDiff, LineKind};
use keleuo_core::links::{self, FileIndex};
use keleuo_core::compose::{self, ListBreak};
use keleuo_core::paste;
use keleuo_core::summary;
use keleuo_core::conversation::{one_line, to_markdown};

mod agents;
mod composer;
mod items;
mod tools;
mod rows;
mod find;
mod latest;

use crate::actions::CopyConversation;
use crate::agent_session::AgentSession;

pub enum ChatPanelEvent {
    OpenFile(PathBuf, Option<u32>),
    /// Open the file as a diff of what the session changed in it.
    OpenDiff(PathBuf),
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
    /// Show the Changes panel.
    ShowChanges,
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
    /// The list's rows (an item each, a long reply several: see `rows`), how many items they
    /// were made from, and each long reply's pieces.
    rows: Vec<rows::Row>,
    synced_items: usize,
    pieces: rows::PieceCache,
    /// Earlier parts being read (by their start in the session file).
    loading: HashSet<u64>,
    /// Images to send with the next message.
    attachments: Vec<PathBuf>,
    /// Long pastes in the message box, shown there as tokens (`[Pasted text #1 · …]`, see
    /// [`keleuo_core::paste`]); paste `n` is `pastes[n - 1]`.
    pastes: Vec<String>,
    /// "Other model…": a field for a model name in place of the model button.
    model_input: Option<(Entity<InputState>, Subscription)>,
    /// The model list box (see `sync_model_select`).
    model_select: Option<composer::ModelSelect>,
    /// What a message starting with `/` can name (skills, commands, the agent's own), read
    /// when the first `/` is typed.
    slash_choices: Option<Rc<Vec<keleuo_core::slash::Choice>>>,
    /// The highlighted one in the `/` list, and the query it was for.
    slash_ix: usize,
    slash_query: String,
    /// Escape closed the list for this query.
    slash_closed: bool,
    /// The theme the message box's Markdown was coloured for (dark or not).
    /// The theme (dark or not) and the Settings switch the Markdown marks were made for.
    marked_as: Option<(bool, bool)>,
    /// The message box's Markdown styles (made the first time there is something to mark).
    md_marks: Option<TextDecorationCollection<TextareaMode>>,
    /// A syntax highlighter per language named on a code fence in the message box.
    md_highlighters: HashMap<String, gpui_kit::component::highlighter::SyntaxHighlighter>,
    /// The message box's spelling checker (made the first time there is something to check).
    spelling: Option<crate::spelling::Spelling>,
    /// Its marks under misspelled words.
    spell_marks: Option<gpui_kit::base::input::RangeDecorationCollection<TextareaMode>>,
    /// The next check, a moment after the last change.
    spell_check: Option<Task<()>>,
    /// The word left unchecked by the last check as being typed, and the length of the text
    /// then: checked when the cursor leaves it.
    spell_held: Option<(std::ops::Range<usize>, usize)>,
    /// The Settings switch as last seen (turning it on or off checks again).
    spelled_as: Option<bool>,
    /// Choices made on an open question card, by request: the labels picked for each question.
    picks: HashMap<String, Vec<Vec<String>>>,
    /// The known failure whose fixes are being offered.
    fixing: Option<Issue>,
    /// Kinds of failure fixed from this panel (their notices say so).
    fixed: std::collections::HashSet<String>,
    /// Tool cards the user opened (or closed, for diffs which start open).
    toggled: HashSet<String>,
    pub index: Arc<FileIndex>,
    last_revision: u64,
    /// Turns the spinners and the clocks while something runs, a few steps a second; slower,
    /// and drawing nothing, while the chat is not on screen.
    ticker: Option<Task<()>>,
    on_screen: crate::on_screen::OnScreen,
    /// When the agent became busy with the current turn (for the time on the thinking line).
    busy_since: Option<std::time::Instant>,
    /// The step the working line's spinner is at, moved on by the ticker: kept apart from the
    /// panel so a step redraws the line, not the chat (see `render_working_line`).
    pulse: Entity<Pulse>,
    /// Where the working line goes, as last laid out (window coordinates).
    working_slot: Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
    /// The working line's spinner, the system's own where there is one.
    native_spinner: crate::native_spinner::NativeSpinner,
    /// The panel itself, for what the workspace draws for it (the working line's box).
    me: WeakEntity<ChatPanel>,
    /// Find in the conversation, while its bar shows; the item of the match gone to (marked);
    /// a match waiting for its part to be read in (where the part starts, its item, the offset).
    find: Option<find::FindBar>,
    found: Option<usize>,
    /// While a row draws with the find bar open: the marks for its text (see `find_marks`).
    marking: Option<gpui_kit::base::text::FindMarks>,
    pending_find: Option<(u64, usize, usize)>,
    /// The latest messages, kept in view under the conversation while it is scrolled up.
    latest: latest::Latest,
    /// The Chat and Terminal view tabs.
    views: Rc<StripState>,
    _subs: Vec<Subscription>,
}

/// How far beyond the visible area the list builds items, so scrolling shows them at once.
const OVERDRAW: f32 = 600.;
/// Items at the end measured again on every change (text streaming in, a result arriving).
const TAIL: usize = 4;
/// The space between the pieces of a reply in pieces: a paragraph's.
const PIECE_GAP: f32 = 12.;

impl EventEmitter<ChatPanelEvent> for ChatPanel {}

/// How often the chat redraws while something runs: the spinners' step and the clocks.
/// Ten a second reads as motion; continuous animation would redraw every frame.
const TICK: std::time::Duration = std::time::Duration::from_millis(100);
/// The ticker's pace while the chat is not shown: enough to notice the work has ended.
const UNSEEN_TICK: std::time::Duration = std::time::Duration::from_secs(1);
const TICKS_PER_SECOND: u32 = 10;
/// The pause in typing after which the message box's spelling is checked.
const SPELL_PAUSE: std::time::Duration = std::time::Duration::from_millis(150);

/// The working line spinner's step.
pub struct Pulse(u32);
/// Steps in a spinner's turn (one turn a little over a second at the tick above).
const SPINNER_STEPS: u32 = 12;
/// Space below the Chat / Terminal tabs, before the window's bottom edge.
const VIEW_STRIP_GAP_BELOW: f32 = 6.;

impl ChatPanel {
    pub fn new(
        session: Entity<AgentSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Signed out: the card shows as the chat opens, before anything is sent.
        session.update(cx, |s, cx| s.check_sign_in(cx));
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
                match ev {
                    InputEvent::PressEnter { shift: false, .. } => panel.send_composed(window, cx),
                    // Any edit clears the box's styles: mark the Markdown again, and check
                    // the spelling once typing pauses.
                    InputEvent::Change => {
                        panel.mark_markdown(cx);
                        panel.check_spelling_soon(SPELL_PAUSE, window, cx);
                    }
                    // Leaving the box: the word being typed is done.
                    InputEvent::Blur => panel.check_spelling_soon(std::time::Duration::ZERO, window, cx),
                    _ => {}
                }
            },
        );
        // The cursor left the word held back as being typed: it is checked now.
        let s3 = cx.observe_in(&input, window, |panel, input, window, cx| {
            if let Some((held, len)) = &panel.spell_held {
                let input = input.read(cx);
                let cursor = input.selected_range().start;
                // A change to the text is not a move: that check comes after the pause.
                if input.value().len() == *len && (cursor < held.start || cursor > held.end) {
                    panel.spell_held = None;
                    panel.check_spelling_soon(std::time::Duration::ZERO, window, cx);
                }
            }
        });
        let s2 = cx.observe(&session, |panel, session, cx| {
            match (session.read(cx).chat.busy, panel.busy_since) {
                (true, None) => panel.busy_since = Some(std::time::Instant::now()),
                (false, Some(_)) => panel.busy_since = None,
                _ => {}
            }
            let rev = session.read(cx).chat.revision;
            if rev != panel.last_revision {
                panel.last_revision = rev;
                panel.sync_rows(cx);
                let running = clocks_running(&session.read(cx).chat);
                if running && panel.ticker.is_none() {
                    panel.ticker = Some(cx.spawn(
                        async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                            let mut wait = TICK;
                            let mut ticks: u32 = 0;
                            let native = crate::native_spinner::NativeSpinner::available();
                            loop {
                                cx.background_executor().timer(wait).await;
                                let alive = this.update(cx, |p, cx| {
                                    let session = p.session.read(cx);
                                    let still = clocks_running(&session.chat);
                                    // Drawn only where the clocks show: on screen, in Chat.
                                    let shown = p.on_screen.now() && session.view == View::Chat;
                                    if shown {
                                        ticks = ticks.wrapping_add(1);
                                        // The working line turns on every tick, drawn apart from
                                        // the chat (where the system turns its own spinner, it
                                        // needs no tick); the chat's own clocks (running calls,
                                        // agents) move once a second.
                                        if !native && p.working_label(cx).is_some() {
                                            p.pulse.update(cx, |pulse, cx| {
                                                pulse.0 = pulse.0.wrapping_add(1);
                                                cx.notify();
                                            });
                                        }
                                        if native || ticks.is_multiple_of(TICKS_PER_SECOND) {
                                            cx.notify();
                                        }
                                    }
                                    if !still {
                                        p.ticker = None;
                                    }
                                    (still, shown)
                                });
                                match alive {
                                    Ok((true, shown)) => wait = if shown && !native { TICK } else { UNSEEN_TICK },
                                    _ => break,
                                }
                            }
                        },
                    ));
                }
            }
            // The view (Chat or Terminal) may have changed too.
            cx.notify();
        });
        let panel = Self {
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
            rows: Vec::new(),
            synced_items: 0,
            pieces: Default::default(),
            loading: HashSet::new(),
            attachments: Vec::new(),
            pastes: Vec::new(),
            model_input: None,
            model_select: None,
            slash_choices: None,
            slash_ix: 0,
            slash_query: String::new(),
            slash_closed: false,
            marked_as: None,
            md_marks: None,
            md_highlighters: HashMap::new(),
            spelling: None,
            spell_marks: None,
            spell_check: None,
            spell_held: None,
            spelled_as: None,
            picks: HashMap::new(),
            fixing: None,
            fixed: Default::default(),
            toggled: HashSet::new(),
            index: Arc::new(FileIndex::default()),
            last_revision: 0,
            busy_since: None,
            pulse: cx.new(|_| Pulse(0)),
            working_slot: Default::default(),
            native_spinner: Default::default(),
            me: cx.entity().downgrade(),
            find: None,
            found: None,
            marking: None,
            pending_find: None,
            latest: latest::Latest::new(
                cx.try_global::<crate::AppSettings>().map_or(latest::DEFAULT_HEIGHT, |s| s.0.borrow().layout.latest_height),
            ),
            ticker: None,
            on_screen: Default::default(),
            views: Rc::new(StripState::default()),
            _subs: vec![s1, s2, s3],
        };
        Self::watch_scrolling(&panel.list, panel.latest.shown.clone(), panel.me.clone());
        panel
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

    /// A spinner at the panel's step: turned by the ticker, a few steps a second, rather
    /// than animated every frame (an animation redraws the whole chat on every frame).
    /// Inside the chat: still, beside a clock that moves each second. Turning it would lay the
    /// whole chat out on every step; the working line at the foot turns (see `spinner_at`).
    fn spinner(&self, color: Hsla) -> Icon {
        Icon::new(IconName::LoaderCircle).text_color(color)
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

    /// The rows for the items shown now: a watched subagent's an item each, the conversation's
    /// with its long replies in pieces.
    fn make_rows(&mut self, cx: &App) -> (Vec<rows::Row>, usize) {
        let session = self.session.clone();
        let chat = &session.read(cx).chat;
        match &self.agent {
            Some(id) => {
                let n = chat.agent(id).map_or(0, |a| a.items.len());
                ((0..n).map(rows::Row::whole).collect(), n)
            }
            None => (rows::rows(&chat.items, &mut self.pieces), chat.items.len()),
        }
    }

    /// Tell the list what changed: rows at the end (new items, a reply's new pieces), and the
    /// last few items' rows measured again in place (they may have grown: their heights stay as
    /// hints and the scroll position stays). A shorter conversation (reloaded) starts over.
    fn sync_rows(&mut self, cx: &App) {
        let (new, items) = self.make_rows(cx);
        if items < self.synced_items {
            self.pieces.clear();
            let (new, _) = self.make_rows(cx);
            self.list.reset(new.len());
            self.list_len = new.len();
            self.rows = new;
            self.synced_items = items;
            self.latest.from = usize::MAX;
            self.sync_latest(0, cx);
            return;
        }
        let old_len = self.rows.len();
        let from_item = self.synced_items.saturating_sub(TAIL);
        let from = self.rows.iter().position(|r| r.item >= from_item).unwrap_or(old_len);
        let new_len = new.len();
        if from < old_len.min(new_len) {
            self.list.remeasure_items(from..old_len.min(new_len));
        }
        if new_len > old_len {
            self.list.splice(old_len..old_len, new_len - old_len);
        } else if new_len < old_len {
            self.list.splice(new_len.max(from)..old_len, 0);
        }
        self.list_len = new_len;
        self.rows = new;
        self.synced_items = items;
        self.sync_latest(from, cx);
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
                    s.attach_summaries();
                    s.chat.revision += 1;
                    s.chat.revision
                });
                let top = p.list.logical_scroll_top();
                // Its row, and the rows of what replaced it (the items after it moved down: the
                // pieces kept by index are made again).
                let at = p.rows.iter().position(|r| r.item == ix).unwrap_or(ix);
                p.pieces.clear();
                let (rows, items) = p.make_rows(cx);
                let added = rows.iter().filter(|r| r.item >= ix && r.item < ix + n).count();
                p.list.splice(at..at + 1, added);
                p.list_len = rows.len();
                p.rows = rows;
                p.synced_items = items;
                p.sync_latest(0, cx);
                // Reached by scrolling up: carry on from its newest item. Jumped to the very top:
                // stay at its start.
                if top.item_ix == at && at > 0 && added > 0 {
                    p.list.scroll_to(ListOffset { item_ix: at + added - 1, offset_in_item: px(0.) });
                }
                p.last_revision = rev;
                p.earlier_read_in(from, ix, cx);
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

    /// One item of the conversation, for the list.
    fn render_entry(&mut self, row: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(r) = self.rows.get(row).cloned() else { return div().into_any_element() };
        let ix = r.item;
        let item = self.item_at(ix, cx);
        let Some(item) = item else { return div().into_any_element() };
        self.marking = self.find_marks(&r, cx);
        let el = self.render_row(&r, row, item, cx);
        self.marking = None;
        el
    }

    fn render_row(&mut self, r: &rows::Row, row: usize, item: Item, cx: &mut Context<Self>) -> AnyElement {
        let ix = r.item;
        let last = row + 1 == self.list_len;
        // A piece of a long reply: its text from the reply, close under the piece before.
        if r.is_piece()
            && let Item::Assistant { text, streaming } = &item
        {
            let top = if r.part == 0 { self.space_above(ix, cx) } else { px(PIECE_GAP) };
            let piece = self.render_reply_piece(ix, r, text, *streaming, cx);
            let el = self.column(piece, last, top);
            return self.mark_found(ix, el, cx);
        }
        let top = self.space_above(ix, cx);
        // The end-of-turn card, turned off in Settings: nothing, taking no room (but the last
        // entry keeps the gap at the bottom).
        if (matches!(item, Item::TurnChanges(_)) && !crate::prefs(cx).turn_card)
            || (matches!(item, Item::Summary { .. }) && !crate::summaries_on(cx))
        {
            return div().when(last, |d| d.pb_4()).into_any_element();
        }
        if let Item::Earlier { from, to, .. } = item {
            // In view (or about to be): read it.
            self.load_earlier(from, to, cx);
        }
        // In a run of tool calls: the first entry is the group's header, the others show
        // only while it is open (closed, they take no room, but the last keeps the bottom gap).
        let mut member = false;
        let mut group = None;
        if let Some((run, key)) = self.tool_group(ix, cx) {
            let open = self.toggled.contains(&key);
            if ix == run.start {
                group = Some(self.render_tool_group(run, key, open, cx));
            }
            if !open {
                return match group {
                    Some(header) => self.column(header, last, top),
                    None => div().when(last, |d| d.pb_4()).into_any_element(),
                };
            }
            member = true;
        }
        let parts = match self.preview_text(ix, &item, cx) {
            Some(text) => vec![self.render_preview(text, cx)],
            None => self.render_items(std::slice::from_ref(&item), &format!("main{ix}"), cx),
        };
        let body = div()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .when(member, |d| d.pl_4())
            .children(parts);
        let el = match group {
            Some(header) => self.column(div().w_full().flex().flex_col().gap_2().child(header).child(body).into_any_element(), last, top),
            None => self.column(body.into_any_element(), last, top),
        };
        self.mark_found(ix, el, cx)
    }

    /// The match gone to, marked: its message on a faint accent ground.
    fn mark_found(&self, ix: usize, el: AnyElement, cx: &App) -> AnyElement {
        if self.find.is_none() || self.found != Some(ix) {
            return el;
        }
        div().w_full().bg(cx.theme().ring.opacity(0.10)).child(el).into_any_element()
    }

    /// The space above entry `ix`. A new turn (the person's message, or the images that lead
    /// it) stands apart from the turn before by three times the space between the items of a
    /// turn: space rather than a rule, since the bubble on the right already marks the start
    /// and a line across the page would cut the conversation into boxes. A finished reply
    /// keeps room under it for its Copy button; that room counts toward the space.
    fn space_above(&self, ix: usize, cx: &App) -> Pixels {
        let chat = &self.session.read(cx).chat;
        let items: &[Item] = match &self.agent {
            Some(id) => chat.agent(id).map(|a| a.items.as_slice()).unwrap_or_default(),
            None => &chat.items,
        };
        let prev = ix.checked_sub(1).and_then(|p| items.get(p));
        let turn_start = ix > 0
            && match items.get(ix) {
                Some(Item::Images(_)) => true,
                Some(Item::User(_)) => !matches!(prev, Some(Item::Images(_))),
                _ => false,
            };
        let want = if turn_start { TURN_GAP } else { ITEM_GAP };
        let kept = match prev {
            Some(Item::Assistant { streaming: false, .. }) => items::HOVER_ROW,
            _ => 0.,
        };
        px((want - kept).max(0.))
    }

    /// An entry in the centred column of comfortable width, the page behind it.
    fn column(&self, content: AnyElement, last: bool, top: Pixels) -> AnyElement {
        div()
            .w_full()
            .flex()
            .justify_center()
            .px_4()
            .pt(top)
            .when(last, |d| d.pb_4())
            .child(div().w_full().max_w(px(COLUMN)).flex().flex_col().gap_2().child(content))
            .into_any_element()
    }

    /// The run of tool calls item `ix` is in (see [`keleuo_core::conversation::tool_run`]),
    /// and the key its open state is kept under.
    fn tool_group(&self, ix: usize, cx: &App) -> Option<(std::ops::Range<usize>, String)> {
        let chat = &self.session.read(cx).chat;
        let items = match &self.agent {
            Some(id) => &chat.agent(id)?.items,
            None => &chat.items,
        };
        // A subagent's card and a call with a fix to offer stay on their own.
        let run = keleuo_core::conversation::tool_run(items, ix, |t| chat.agent(&t.id).is_none() && t.issue.is_none())?;
        let Some(Item::Tool(first)) = items.get(run.start) else { return None };
        let key = format!("group:{}", first.id);
        Some((run, key))
    }

    /// Scroll to the reply that holds `text` (the latest one, in the list shown).
    pub(super) fn reveal_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let chat = &self.session.read(cx).chat;
        let items = match &self.agent {
            Some(id) => chat.agent(id).map(|a| a.items.as_slice()).unwrap_or_default(),
            None => chat.items.as_slice(),
        };
        let found = items.iter().rposition(|i| matches!(i, Item::Assistant { text: t, .. } if t.contains(text)));
        if let Some(ix) = found {
            // The piece holding it, in a reply in pieces.
            let reply = match &items[ix] {
                Item::Assistant { text: t, .. } => t.clone(),
                _ => String::new(),
            };
            let at = reply.find(text).unwrap_or(0);
            let row = self
                .rows
                .iter()
                .position(|r| r.item == ix && (!r.is_piece() || r.range.contains(&at)))
                .or_else(|| self.rows.iter().position(|r| r.item == ix))
                .unwrap_or(ix);
            self.list.scroll_to(ListOffset { item_ix: row, offset_in_item: px(0.) });
            cx.notify();
        }
    }

    pub fn focus_input(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |s, cx| s.focus(window, cx));
    }

    fn open(&self, path: PathBuf, line: Option<u32>, cx: &mut Context<Self>) {
        cx.emit(ChatPanelEvent::OpenFile(path, line));
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, hover) = (theme.muted_foreground, theme.secondary_hover);
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
        let current = session.spend(cx);
        let showing_context = matches!(current, Some(Spend::Context { .. }));
        let (spend, spend_tip) = match current {
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
            // How much of the model's context the conversation fills: "180k of 1M context".
            Some(Spend::Context { tokens, window, fill, .. }) => (
                Some(format!("{} of {} context", short_tokens(tokens), short_tokens(window))),
                match fill {
                    Fill::Room => "The conversation as of the last request, against the model's context window",
                    Fill::Filling => "The conversation as of the last request, against the model's context window: filling up",
                    Fill::Nearly => "The conversation as of the last request, against the model's context window: the agent compacts it soon",
                },
            ),
            None => (None, ""),
        };
        // Amber, then red, on the way to where the agent compacts by itself.
        let used = session.context_use();
        let fill = used.map(|u| u.fill()).unwrap_or(Fill::Room);
        let spend_color = match fill {
            Fill::Filling if showing_context => theme.warning,
            Fill::Nearly if showing_context => theme.danger,
            _ => muted,
        };
        let compact = session.backend.compact_command().is_some();
        let offer_compact = compact && fill != Fill::Room;
        let spend_tip = format!(
            "{spend_tip}. Cost shows: {}. Click to change.",
            match setting.as_str() {
                "always" => "always",
                "never" => "never (context instead)",
                _ => "auto (only when billed per use)",
            }
        );
        let weak = cx.entity().downgrade();
        let weak_session = self.session.downgrade();
        // Where its agent may write, for the menu: kept to the folder (and the folders allowed
        // too), or anywhere; nothing when it cannot be kept (Settings, this system, SSH).
        let writes = session.can_confine(cx).then(|| (session.writes_anywhere, session.writable.clone()));
        let sign_in = session.backend.sign_in().map(|_| format!("Sign in to {}…", session.backend.name()));
        // Sessions it may message (or be messaged by) without asking: a click asks again.
        let allowed = crate::messaging::allowed_with(&self.session, cx);
        // As tall as a tool panel's title bar, a hairline under it.
        div()
            .h(px(crate::dock_skin::HEADER_HEIGHT))
            .flex_none()
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(13.))
            .text_color(muted)
            .bg(cx.theme().background)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(Icon::new(IconName::Bot).small())
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
                        .text_color(spend_color)
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
            // Once the context fills up, compacting is a click away (agents that can be asked).
            .when(offer_compact, |d| {
                let session = self.session.downgrade();
                d.child(
                    div()
                        .id("compact")
                        .flex_none()
                        .px_2()
                        .rounded_full()
                        .border_1()
                        .border_color(spend_color)
                        .text_color(spend_color)
                        .text_size(px(12.))
                        .cursor_pointer()
                        .hover(|d| d.bg(hover))
                        .child("Compact")
                        .tooltip(|w, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(
                                "Summarise the conversation so far to free up context (sends /compact; waits for the turn to end)",
                            )
                            .build(w, cx)
                        })
                        .on_click(move |_, _, cx| {
                            let _ = session.update(cx, |s, cx| s.compact(cx));
                        }),
                )
            })
            .child(
                Button::new("chat-find")
                    .icon(IconName::Search)
                    .xsmall()
                    .ghost()
                    .tab_stop(false)
                    .tooltip("Find in conversation (Cmd+F)")
                    .on_click(cx.listener(|p, _, window, cx| {
                        if p.find.is_some() {
                            p.close_find(window, cx)
                        } else {
                            p.open_find(window, cx)
                        }
                    })),
            )
            .child(
                Button::new("chat-options")
                    .icon(IconName::Ellipsis)
                    .xsmall()
                    .ghost()
                    .tab_stop(false)
                    .dropdown_menu(move |menu, _, _| {
                        let (weak, panel) = (weak.clone(), weak.clone());
                        let session = weak_session.clone();
                        menu.item(
                            gpui_kit::component::menu::PopupMenuItem::new("Copy conversation (Cmd+Alt+C)").on_click(
                                move |_, window, cx| {
                                    let _ = weak.update(cx, |p, cx| p.copy_conversation(window, cx));
                                },
                            ),
                        )
                        .when(compact, |menu| {
                            let session = session.clone();
                            menu.item(gpui_kit::component::menu::PopupMenuItem::new("Compact the context").on_click(
                                move |_, _, cx| {
                                    let _ = session.update(cx, |s, cx| s.compact(cx));
                                },
                            ))
                        })
                        .when_some(sign_in.clone(), |menu, label| {
                            let (panel, session) = (panel.clone(), session.clone());
                            menu.item(gpui_kit::component::menu::PopupMenuItem::new(label).on_click(move |_, _, cx| {
                                let line = session.update(cx, |s, cx| s.start_sign_in(cx)).ok().flatten();
                                if let Some(line) = line {
                                    let _ = panel.update(cx, |_, cx| cx.emit(ChatPanelEvent::RunCommand(line)));
                                }
                            }))
                        })
                        .when_some(writes.clone(), |menu, (anywhere, writable)| write_items(menu, session.clone(), anywhere, writable))
                        .when(!allowed.is_empty(), |menu| {
                            allowed.iter().fold(menu.separator(), |menu, (me, other, name)| {
                                let (me, other) = (*me, *other);
                                menu.item(gpui_kit::component::menu::PopupMenuItem::new(format!("Ask before messages with {name}")).on_click(
                                    move |_, _, cx| crate::messaging::forget_always(me, other, cx),
                                ))
                            })
                        })
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
        let ground = crate::theme::strip_ground(cx);
        let theme = cx.theme();
        let content = theme.background;
        let palette = Palette::new(ground, theme.foreground, content, theme.foreground, theme.border, theme.ring);
        let shown = match session.view {
            View::Chat => 0,
            View::Terminal => 1,
        };
        let default_terminal = cx.global::<crate::AppSettings>().0.borrow().view_for(None) == "terminal";
        let muted = theme.muted_foreground;
        let locked = session.switch_blocked().map(|why| (SharedString::from(why), self.spinner(muted).xsmall()));
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
            locked,
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

impl Render for ChatPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::count(crate::perf::Count::Chat);
        // Laid out again below, if the working line shows.
        self.working_slot.set(None);
        if let Some(id) = self.agent.clone() {
            return self.render_agent_view(&id, cx);
        }
        // Light and dark changed, or the marks were turned on or off: they follow.
        if self.marked_as != Some((cx.theme().is_dark(), crate::prefs(cx).markdown_marks)) {
            self.mark_markdown(cx);
        }
        if self.spelled_as != Some(crate::prefs(cx).spell_check) {
            self.spelled_as = Some(crate::prefs(cx).spell_check);
            self.check_spelling_soon(std::time::Duration::ZERO, window, cx);
        }
        let started = std::time::Instant::now();
        if crate::perf::enabled() {
            // Build, layout and paint: from here to the frame being on screen.
            window.on_next_frame(move |_, _| crate::perf::record("chat frame (render to frame)", started.elapsed()));
        }
        let theme = cx.theme();
        let (fg, muted, red, user_bg, _border) = (
            theme.foreground,
            theme.muted_foreground,
            theme.red,
            theme.secondary,
            theme.border,
        );
        let backend_name = self.session.read(cx).backend.name();
        if self.item_count(cx) != self.synced_items {
            self.sync_rows(cx);
        }
        let empty = self.list_len == 0;
        let _ = (fg, red, user_bg);
        let terminal = {
            let s = self.session.read(cx);
            (s.view == View::Terminal).then(|| s.terminal_view()).flatten()
        };
        crate::perf::record("chat build (elements)", started.elapsed());
        crate::perf::switch_shown(window);
        let font = crate::fonts::chat(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .font_weight(crate::fonts::weight(&font))
            .font_family(font)
            .bg(cx.theme().background)
            .key_context("Chat")
            .track_focus(&self.focus)
            .on_action(cx.listener(|p, _: &CopyConversation, window, cx| p.copy_conversation(window, cx)))
            // Find: its own key, and the message box's (Cmd+F there asks the box to search, and a
            // box that does not passes it on).
            .on_action(cx.listener(|p, _: &crate::actions::FindInChat, window, cx| p.open_find(window, cx)))
            .on_action(cx.listener(|p, _: &gpui_kit::component::input::Search, window, cx| p.open_find(window, cx)))
            .child(self.render_header(cx))
            .children(self.render_find(cx))
            // The agent's terminal as a view of its own, cached: drawing the chat (its header,
            // a clock) does not paint the whole grid again; the terminal does when it changes.
            .when_some(terminal.clone(), |d, term| {
                d.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .relative()
                        .child(AnyView::from(term).cached(StyleRefinement::default().size_full())),
                )
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
            .children(self.render_latest(cx))
            .children(self.render_working_line(cx))
            .children(self.render_asking(cx))
            .child({
                let slash = self.render_slash(cx);
                self.sync_model_select(window, cx);
                self.render_composer(slash, window, cx)
            }))
            .children(self.render_views(cx))
            .children(self.render_fix(cx))
            // Marks the chat as on screen while a frame shows it (see `on_screen`).
            .child({
                let on_screen = self.on_screen.clone();
                canvas(|_, _, _| {}, move |_, _, window, _| on_screen.mark(window)).absolute().size_0()
            })
            .relative()
    }
}

/// A token count as said briefly: 950, 18k, 1.2M, 1M.
fn short_tokens(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => {
            let m = n as f64 / 1_000_000.0;
            if (m - m.round()).abs() < 0.05 { format!("{:.0}M", m) } else { format!("{m:.1}M") }
        }
        n if n >= 1_000 => format!("{}k", (n + 500) / 1_000),
        n => n.to_string(),
    }
}

/// The column's widest.
const COLUMN: f32 = 760.;

/// The space between the items of one turn, and above the person's message that starts the
/// next: three times as much, so turns read as paragraphs of the conversation.
const ITEM_GAP: f32 = 12.;
const TURN_GAP: f32 = 36.;

/// Something live runs: a subagent, a tool call, or a turn's summary being made.
fn clocks_running(chat: &keleuo_core::conversation::Conversation) -> bool {
    chat.busy
        || chat.running_agents().next().is_some()
        || chat.running_tools().next().is_some()
        // A turn's summary being made shows a spinner too.
        || chat.items.iter().rev().any(|i| matches!(i, Item::Summary { state: summary::State::Pending, .. }))
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

#[cfg(test)]
mod context_label_tests {
    #[::core::prelude::v1::test]
    fn token_counts_read_briefly() {
        assert_eq!(super::short_tokens(950), "950");
        assert_eq!(super::short_tokens(18_400), "18k");
        assert_eq!(super::short_tokens(1_000_000), "1M");
        assert_eq!(super::short_tokens(1_250_000), "1.2M");
        assert_eq!(super::short_tokens(200_000), "200k");
    }
}

/// A spinner at step `phase` of its turn.
fn spinner_at(phase: u32, color: Hsla) -> Icon {
    let turn = (phase % SPINNER_STEPS) as f32 / SPINNER_STEPS as f32;
    Icon::new(IconName::LoaderCircle).text_color(color).transform(Transformation::rotate(percentage(turn)))
}

/// The ⋯ menu's write items: writes anywhere or kept to the folder, the folders allowed too
/// (each a click to stop), and one more to allow (picked).
fn write_items(
    menu: gpui_kit::component::menu::PopupMenu,
    session: WeakEntity<AgentSession>,
    anywhere: bool,
    writable: Vec<PathBuf>,
) -> gpui_kit::component::menu::PopupMenu {
    use gpui_kit::component::menu::PopupMenuItem;
    let toggle = session.clone();
    let mut menu = menu.separator().item(PopupMenuItem::new("Allow writes outside this folder").checked(anywhere).on_click(move |_, _, cx| {
        let _ = toggle.update(cx, |s, cx| s.set_writes_anywhere(!anywhere, cx));
    }));
    if anywhere {
        return menu;
    }
    for folder in writable {
        let (s2, f2) = (session.clone(), folder.clone());
        let name = folder.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| folder.display().to_string());
        menu = menu.item(PopupMenuItem::new(format!("Writes allowed in {name}")).checked(true).on_click(move |_, _, cx| {
            let _ = s2.update(cx, |s, cx| s.set_writable(f2.clone(), false, cx));
        }));
    }
    menu.item(PopupMenuItem::new("Allow writes in a folder…").on_click(move |_, _, cx| {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Allow".into()) });
        let session = session.clone();
        cx.spawn(async move |cx: &mut AsyncApp| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(folder) = paths.into_iter().next() else { return };
            let _ = session.update(cx, |s, cx| s.set_writable(folder, true, cx));
        })
        .detach();
    }))
}
