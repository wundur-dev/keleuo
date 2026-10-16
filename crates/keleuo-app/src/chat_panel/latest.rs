//! The latest messages, kept in view while the conversation is scrolled up: a pane under it
//! showing the current turn (from the person's last message), following it as it grows. It
//! shows only while the newest row is out of view (at the bottom it would repeat what shows),
//! and a separator above it sets its height, kept for next time. Settings can turn it off.

use std::cell::Cell;

use super::*;

/// The pane's height when none is kept, and the least it can be.
pub const DEFAULT_HEIGHT: f32 = 220.;
const MIN_HEIGHT: f32 = 72.;
/// The room the conversation above keeps, however high the pane is dragged.
const MIN_ABOVE: f32 = 120.;
/// The separator's grip, taller than the line it draws.
const GRIP: f32 = 7.;
/// The height of the Go to latest pill on the separator.
const PILL: f32 = 30.;

/// The pane's own list and its state, apart from the conversation's list.
pub(super) struct Latest {
    pub list: ListState,
    /// The first of the conversation's rows the pane shows.
    pub from: usize,
    /// Its height, in pixels; and a drag of the separator (where it began, the height then).
    pub height: Rc<Cell<f32>>,
    pub drag: Rc<Cell<Option<(f32, f32)>>>,
    /// Whether the last frame showed it, for the scroll handler to tell when that changes.
    pub shown: Rc<Cell<bool>>,
}

impl Latest {
    pub fn new(height: f32) -> Self {
        let list = ListState::new(0, ListAlignment::Top, px(OVERDRAW));
        list.set_follow_mode(FollowMode::Tail);
        Self {
            list,
            from: usize::MAX,
            height: Rc::new(Cell::new(height.max(MIN_HEIGHT))),
            drag: Rc::default(),
            shown: Rc::default(),
        }
    }
}

impl ChatPanel {
    /// Where the pane starts: the row of the person's last message; without one, the last row.
    fn latest_start(&self, cx: &App) -> usize {
        let chat = &self.session.read(cx).chat;
        let item = chat.items.iter().rposition(|i| matches!(i, Item::User(_)));
        let row = item.and_then(|ix| self.rows.iter().position(|r| r.item == ix));
        row.unwrap_or(self.rows.len().saturating_sub(1)).min(self.rows.len())
    }

    /// Bring the pane's list in step with the rows: from `changed` on they were measured again
    /// or added. A new turn (a new start) starts the list over.
    pub(super) fn sync_latest(&mut self, changed: usize, cx: &App) {
        let start = self.latest_start(cx);
        let count = self.rows.len() - start;
        if start != self.latest.from {
            self.latest.from = start;
            self.latest.list.reset(count);
            return;
        }
        let old = self.latest.list.item_count();
        let from = changed.saturating_sub(start);
        if from < old.min(count) {
            self.latest.list.remeasure_items(from..old.min(count));
        }
        if count > old {
            self.latest.list.splice(old..old, count - old);
        } else if count < old {
            self.latest.list.splice(count.max(from)..old, 0);
        }
    }

    /// Told by the conversation's list as it scrolls: redraw when the pane is to show or go.
    pub(super) fn watch_scrolling(list: &ListState, shown: Rc<Cell<bool>>, me: WeakEntity<Self>) {
        list.set_scroll_handler(move |ev, _, cx| {
            let below = ev.visible_range.end < ev.count;
            if below != shown.get() {
                shown.set(below);
                let _ = me.update(cx, |_, cx| cx.notify());
            }
        });
    }

    /// The pane, when it shows: on in Settings, the conversation (not a subagent's) scrolled
    /// up so that its newest row is out of view.
    pub(super) fn render_latest(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let last = self.list_len.checked_sub(1);
        let show = crate::prefs(cx).latest_pane
            && self.agent.is_none()
            && self.latest.from < self.list_len
            // Rows just added are not measured yet, so not known to be below: then the list not
            // following its end says they are.
            && last.is_some_and(|l| self.list.item_is_below_viewport(l).unwrap_or(!self.list.is_following_tail()));
        self.latest.shown.set(show);
        if !show {
            return None;
        }
        let theme = cx.theme();
        let (border, ground, muted) = (theme.border, theme.background, theme.muted_foreground);
        let (fg, raised, hover) = (theme.foreground, theme.background, theme.secondary_hover);
        let height = self.latest.height.get();
        let dragging = self.latest.drag.get().is_some();
        let from = self.latest.from;
        let (drag, height_cell, conversation) = (self.latest.drag.clone(), self.latest.height.clone(), self.list.clone());
        let separator = div()
            .id("latest-separator")
            .flex_none()
            .h(px(GRIP))
            .w_full()
            .cursor(CursorStyle::ResizeRow)
            .flex()
            .items_center()
            .child(div().h(px(if dragging { 2. } else { 1. })).w_full().bg(if dragging { theme.ring } else { border }))
            .on_mouse_down(MouseButton::Left, {
                let (drag, height) = (drag.clone(), height_cell.clone());
                move |e: &MouseDownEvent, _, cx| {
                    drag.set(Some((f32::from(e.position.y), height.get())));
                    cx.stop_propagation();
                }
            })
            // While dragged, the pointer is followed anywhere in the window until let go; the
            // height is kept then.
            .child(
                canvas(|_, _, _| (), move |_, _, window, _| {
                    if drag.get().is_none() {
                        return;
                    }
                    let view = window.current_view();
                    let (moving, letting_go, height, list) = (drag.clone(), drag.clone(), height_cell.clone(), conversation.clone());
                    window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                        let Some((y0, h0)) = moving.get() else { return };
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        if e.pressed_button != Some(MouseButton::Left) {
                            moving.set(None);
                            cx.notify(view);
                            return;
                        }
                        // Up makes it taller; the conversation above keeps some room.
                        let room = f32::from(list.viewport_bounds().size.height) + height.get() - MIN_ABOVE;
                        let h = (h0 + y0 - f32::from(e.position.y)).clamp(MIN_HEIGHT, room.max(MIN_HEIGHT));
                        if h != height.get() {
                            height.set(h);
                            cx.notify(view);
                        }
                    });
                    let kept = height_cell.clone();
                    window.on_mouse_event(move |_: &MouseUpEvent, _, _, cx| {
                        if letting_go.take().is_some() {
                            let h = kept.get();
                            crate::settings_window::change(cx, |s| s.layout.latest_height = h);
                        }
                    });
                })
                .absolute()
                .size_0(),
            );
        let list = list(self.latest.list.clone(), cx.processor(move |this, ix, _window, cx| this.render_entry(from + ix, cx))).size_full();
        Some(
            div()
                .flex_none()
                .h(px(height))
                .w_full()
                .flex()
                .flex_col()
                .bg(ground)
                .child(separator)
                .child(
                    div()
                        .flex_none()
                        .px_3()
                        .flex()
                        .items_center()
                        .justify_between()
                        .text_xs()
                        .text_color(muted)
                        .child("Latest"),
                )
                // Its own place among the window's elements: its messages keep their own state
                // (selection, layout), apart from the same messages in the conversation above.
                .child(div().id("latest").flex_1().min_h_0().child(list))
                // One view with the conversation: the wheel anywhere over the pane scrolls the
                // conversation above, taken before the pane's list sees it, so the pane stays on
                // the end of the turn.
                .child({
                    let (conversation, view) = (self.list.clone(), cx.entity_id());
                    canvas(|_, _, _| (), move |bounds, _, window, _| {
                        let conversation = conversation.clone();
                        window.on_mouse_event(move |e: &ScrollWheelEvent, phase, _, cx| {
                            if phase != DispatchPhase::Capture || !bounds.contains(&e.position) {
                                return;
                            }
                            cx.stop_propagation();
                            // As the list scrolls itself: a wheel's lines at 20 px; down is less.
                            let dy = -e.delta.pixel_delta(px(20.)).y;
                            conversation.scroll_by(dy);
                            // Back down at the end: follow it again (the pane then goes).
                            if dy > px(0.) && conversation.is_scrolled_to_end() == Some(true) {
                                conversation.set_follow_mode(FollowMode::Tail);
                            }
                            cx.notify(view);
                        });
                    })
                    .absolute()
                    .size_full()
                })
                // Down to the end of the conversation (which puts the pane away) and following
                // it again: a pill on the separator's line, over both sides of it. Apart from
                // the separator, so pressing it does not start a drag.
                .child(
                    div()
                        .absolute()
                        .top(px(GRIP / 2. - PILL / 2.))
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .id("latest-go")
                                .h(px(PILL))
                                .px_3()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .rounded_full()
                                .border_1()
                                .border_color(border)
                                .bg(raised)
                                .shadow_md()
                                .text_sm()
                                .text_color(fg)
                                .cursor_pointer()
                                .hover(move |d| d.bg(hover))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(|p, _, _, cx| {
                                    p.list.set_follow_mode(FollowMode::Tail);
                                    cx.notify();
                                }))
                                .child(Icon::new(IconName::ArrowDown).small())
                                .child("Go to latest"),
                        ),
                )
                .relative()
                .into_any_element(),
        )
    }
}
