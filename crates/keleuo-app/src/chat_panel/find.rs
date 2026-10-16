//! Find in the conversation (Cmd+F, Ctrl+F, the header's search button): a bar under the
//! header with a field, how many matches and which, and the way to the previous and next. It
//! searches the conversation's data, not what is drawn: a reply out of view is found the same,
//! and the parts of a long session not read yet are read for it, off the UI thread. Going to a
//! match scrolls the list to it (reading its part in first when it is one of those) and marks
//! its message.

use std::collections::HashMap;

use keleuo_core::find;

use super::*;

/// One place the query occurs: in item `ix` of the list, or in item `ix` of a part of the
/// session not read into the list yet (`from..to`), `at` bytes into the item's text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Hit {
    Item { ix: usize, at: usize },
    Earlier { from: u64, to: u64, ix: usize, at: usize },
}

pub(super) struct FindBar {
    pub input: Entity<InputState>,
    /// The words last searched for.
    pub query: String,
    pub hits: Vec<Hit>,
    pub current: Option<usize>,
    /// The parts not read into the list, read for searching, by where they start.
    pub earlier: HashMap<u64, Vec<Item>>,
    pub reading: bool,
    /// The conversation's revision when it was last searched.
    pub searched: u64,
    _sub: Subscription,
}

impl ChatPanel {
    /// Show the bar (its field focused, its text selected), or focus it when it shows.
    pub(super) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.read(cx).view == View::Terminal {
            return;
        }
        if self.find.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in conversation"));
            let sub = cx.subscribe_in(&input, window, |p, input, ev: &InputEvent, window, cx| match ev {
                // Searched again only when the words changed (the field reports a change on Enter too).
                InputEvent::Change => {
                    let query = input.read(cx).value().to_string();
                    if p.find.as_ref().is_some_and(|f| f.query != query) {
                        p.run_find(true, cx);
                    }
                }
                InputEvent::PressEnter { shift, .. } => p.find_step(if *shift { -1 } else { 1 }, window, cx),
                _ => {}
            });
            self.find = Some(FindBar {
                input,
                query: String::new(),
                hits: Vec::new(),
                current: None,
                earlier: HashMap::new(),
                reading: false,
                searched: 0,
                _sub: sub,
            });
        }
        if let Some(f) = &self.find {
            let input = f.input.clone();
            input.update(cx, |i, cx| {
                i.focus(window, cx);
                i.select_all(window, cx);
            });
        }
        cx.notify();
    }

    pub(super) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find = None;
        self.found = None;
        self.pending_find = None;
        self.focus_input(window, cx);
        cx.notify();
    }

    /// The items searched: the watched subagent's, or the conversation's.
    fn find_items<'a>(&self, chat: &'a keleuo_core::conversation::Conversation) -> &'a [Item] {
        match &self.agent {
            Some(id) => chat.agent(id).map(|a| a.items.as_slice()).unwrap_or_default(),
            None => &chat.items,
        }
    }

    /// Search again: every item, and every part not read into the list that has been read
    /// for searching (the others are read now, and searched when they come). `fresh`: the
    /// query changed, so the newest match is the current one; else the current one stays.
    pub(super) fn run_find(&mut self, fresh: bool, cx: &mut Context<Self>) {
        let Some(f) = &self.find else { return };
        let query = f.input.read(cx).value().to_string();
        let session = self.session.clone();
        let chat = &session.read(cx).chat;
        let items = self.find_items(chat);
        let mut hits = Vec::new();
        let mut unread = Vec::new();
        for (ix, item) in items.iter().enumerate() {
            if let Item::Earlier { from, to, .. } = item {
                match f.earlier.get(from) {
                    Some(part) => {
                        for (j, it) in part.iter().enumerate() {
                            for at in find::occurrences(&find::item_text(it), &query) {
                                hits.push(Hit::Earlier { from: *from, to: *to, ix: j, at });
                            }
                        }
                    }
                    None => unread.push((*from, *to)),
                }
                continue;
            }
            for at in find::occurrences(&find::item_text(item), &query) {
                hits.push(Hit::Item { ix, at });
            }
        }
        let revision = chat.revision;
        let f = self.find.as_mut().expect("checked above");
        let previous = f.current.and_then(|c| f.hits.get(c).copied());
        f.current = match (fresh, previous) {
            (false, Some(p)) => hits.iter().position(|h| *h == p).or_else(|| hits.len().checked_sub(1)),
            _ => hits.len().checked_sub(1),
        };
        f.hits = hits;
        f.searched = revision;
        f.query = query.clone();
        let read_more = !query.trim().is_empty() && !unread.is_empty() && !f.reading && self.agent.is_none();
        if fresh && let Some(c) = f.current {
            let hit = f.hits[c];
            self.go_to(hit, cx);
        } else if f.hits.is_empty() {
            self.found = None;
        }
        if read_more {
            self.read_for_find(unread, cx);
        }
        cx.notify();
    }

    /// Read the parts not in the list yet, off the UI thread, then search again with them.
    fn read_for_find(&mut self, parts: Vec<(u64, u64)>, cx: &mut Context<Self>) {
        let Some(load) = self.session.read(cx).earlier_loader_many() else { return };
        if let Some(f) = self.find.as_mut() {
            f.reading = true;
        }
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let read = cx.background_spawn(async move { load(parts) }).await;
            let _ = this.update(cx, |p, cx| {
                let Some(f) = p.find.as_mut() else { return };
                f.reading = false;
                f.earlier.extend(read);
                p.run_find(false, cx);
            });
        })
        .detach();
    }

    /// The previous (`-1`) or next (`1`) match, round the ends.
    pub(super) fn find_step(&mut self, delta: isize, _: &mut Window, cx: &mut Context<Self>) {
        let revision = self.session.read(cx).chat.revision;
        if self.find.as_ref().is_some_and(|f| f.searched != revision) {
            self.run_find(false, cx);
        }
        let Some(f) = self.find.as_mut() else { return };
        let n = f.hits.len();
        if n == 0 {
            return;
        }
        let next = match f.current {
            Some(c) => (c as isize + delta).rem_euclid(n as isize) as usize,
            None if delta < 0 => n - 1,
            None => 0,
        };
        f.current = Some(next);
        let hit = f.hits[next];
        self.go_to(hit, cx);
        cx.notify();
    }

    /// Scroll to a match and mark its message; one in a part not in the list is read in
    /// first (as scrolling up to it would), then gone to.
    fn go_to(&mut self, hit: Hit, cx: &mut Context<Self>) {
        match hit {
            Hit::Item { ix, at } => {
                let row = self
                    .rows
                    .iter()
                    .position(|r| r.item == ix && (!r.is_piece() || r.range.contains(&at) || r.range.end == at))
                    .or_else(|| self.rows.iter().position(|r| r.item == ix));
                if let Some(row) = row {
                    self.list.scroll_to(ListOffset { item_ix: row, offset_in_item: px(0.) });
                }
                self.found = Some(ix);
            }
            Hit::Earlier { from, to, ix, at } => {
                self.pending_find = Some((from, ix, at));
                self.load_earlier(from, to, cx);
            }
        }
    }

    /// A part was read into the list at item `at`: a match waiting in it is gone to, and the
    /// search runs again (its matches are the list's now).
    pub(super) fn earlier_read_in(&mut self, from: u64, at: usize, cx: &mut Context<Self>) {
        if self.find.is_none() {
            return;
        }
        if let Some(f) = self.find.as_mut() {
            f.earlier.remove(&from);
        }
        let waiting = self.pending_find.take_if(|(f, ..)| *f == from);
        self.run_find(false, cx);
        // The part came in above the match gone to, and moved it: mark it where it is now.
        if let Some(Hit::Item { ix, .. }) = self.find.as_ref().and_then(|f| f.current.and_then(|c| f.hits.get(c).copied())) {
            self.found = Some(ix);
        }
        if let Some((_, ix, offset)) = waiting {
            let hit = Hit::Item { ix: at + ix, at: offset };
            if let Some(f) = self.find.as_mut() {
                f.current = f.hits.iter().position(|h| *h == hit).or(f.current);
            }
            self.go_to(hit, cx);
        }
    }

    /// The bar under the header: the field, "3 of 12", previous, next, close.
    pub(super) fn render_find(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let f = self.find.as_ref()?;
        let theme = cx.theme();
        let (muted, border, bg) = (theme.muted_foreground, theme.border, theme.background);
        let query_empty = f.input.read(cx).value().trim().is_empty();
        let count = match (f.current, f.hits.len()) {
            _ if query_empty => String::new(),
            (_, 0) if f.reading => "Searching earlier…".to_string(),
            (_, 0) => "No matches".to_string(),
            (Some(c), n) => format!("{} of {n}{}", c + 1, if f.reading { " so far" } else { "" }),
            (None, n) => format!("{n} matches"),
        };
        let step = |id: &'static str, icon: IconName, delta: isize, tip: &'static str| {
            Button::new(id)
                .icon(icon)
                .xsmall()
                .ghost()
                .tab_stop(false)
                .tooltip(tip)
                .on_click(cx.listener(move |p, _, window, cx| p.find_step(delta, window, cx)))
        };
        Some(
            div()
                .flex_none()
                .h(px(40.))
                .px(px(10.))
                .flex()
                .items_center()
                .gap_2()
                .bg(bg)
                .border_b_1()
                .border_color(border)
                .on_key_down(cx.listener(|p, ev: &KeyDownEvent, window, cx| {
                    if ev.keystroke.key == "escape" {
                        cx.stop_propagation();
                        p.close_find(window, cx);
                    }
                }))
                .child(Icon::new(IconName::Search).small().text_color(muted))
                .child(div().flex_1().min_w_0().child(Input::new(&f.input).small().appearance(false)))
                .child(div().flex_none().text_size(px(12.)).text_color(muted).child(count))
                .child(step("find-previous", IconName::ChevronUp, -1, "Previous match (Shift+Enter)"))
                .child(step("find-next", IconName::ChevronDown, 1, "Next match (Enter)"))
                .child(
                    Button::new("find-close")
                        .icon(IconName::Close)
                        .xsmall()
                        .ghost()
                        .tab_stop(false)
                        .tooltip("Close (Esc)")
                        .on_click(cx.listener(|p, _, window, cx| p.close_find(window, cx))),
                )
                .into_any_element(),
        )
    }
}
