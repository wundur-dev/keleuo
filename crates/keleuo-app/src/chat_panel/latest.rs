//! Go to latest: while the conversation is scrolled up with its newest row out of view, a small
//! pill floats over its bottom and takes it back down (following the end again). It says New
//! messages when something came in since the person scrolled up. It floats over the list, so
//! showing it moves nothing.
//!
//! It replaced a pane that kept the current turn in view under the conversation: the pane
//! showing and going as the list scrolled changed the list's height, which flickered.

use std::cell::Cell;

use super::*;

/// What the scroll handler and the drawing share.
#[derive(Default)]
pub(super) struct Latest {
    /// Whether rows came or grew while the conversation was scrolled up with its newest row
    /// out of view; cleared once the newest row is in view again.
    pub news: Rc<Cell<bool>>,
    /// Whether the last frame showed the pill, for the scroll handler to tell when that changes.
    pub shown: Rc<Cell<bool>>,
}

impl ChatPanel {
    /// Told by the conversation's list as it scrolls: redraw when the pill is to show or go.
    pub(super) fn watch_scrolling(list: &ListState, latest: &Latest, me: WeakEntity<Self>) {
        let (shown, news) = (latest.shown.clone(), latest.news.clone());
        list.set_scroll_handler(move |ev, _, cx| {
            let newest_below = ev.visible_range.end < ev.count;
            if !newest_below {
                news.set(false);
            }
            if newest_below != shown.get() {
                shown.set(newest_below);
                let _ = me.update(cx, |_, cx| cx.notify());
            }
        });
    }

    /// The pill, when it shows: the conversation (not a subagent's) scrolled up so that its
    /// newest row is out of view. Placed over the bottom of the conversation's area.
    pub(super) fn render_latest(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // Rows just added are not measured yet, so not known to be below: then the list not
        // following its end says they are.
        let following = self.list.is_following_tail();
        let newest_below = self.list_len.checked_sub(1).is_some_and(|ix| self.list.item_is_below_viewport(ix).unwrap_or(!following));
        if !newest_below {
            self.latest.news.set(false);
        }
        let show = self.agent.is_none() && newest_below;
        self.latest.shown.set(show);
        if !show {
            return None;
        }
        let theme = cx.theme();
        let (fg, raised, hover, border) = (theme.foreground, theme.background, theme.secondary_hover, theme.border);
        let label = if self.latest.news.get() { "New messages" } else { "Latest" };
        Some(
            div()
                .absolute()
                .bottom(px(10.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("go-to-latest")
                        .h(px(30.))
                        .px(px(12.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
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
                            p.latest.news.set(false);
                            cx.notify();
                        }))
                        .child(Icon::new(IconName::ArrowDown).small())
                        .child(label),
                )
                .into_any_element(),
        )
    }
}
