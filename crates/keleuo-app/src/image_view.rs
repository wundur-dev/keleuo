//! An image shown over the whole window, as large as it fits: a picture in the chat (a pasted
//! one, one sent with a message, a screenshot a tool looked at) opened full size. A Close
//! button, Esc, or a click anywhere closes it.

use std::path::PathBuf;

use gpui_kit::component::{Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;

/// The image shown, if any.
#[derive(Default)]
struct Shown(Option<PathBuf>);
impl Global for Shown {}

pub fn show(path: PathBuf, cx: &mut App) {
    cx.set_global(Shown(Some(path)));
    cx.refresh_windows();
}

pub fn close(cx: &mut App) {
    cx.set_global(Shown(None));
    cx.refresh_windows();
}

pub fn is_open(cx: &App) -> bool {
    cx.try_global::<Shown>().is_some_and(|s| s.0.is_some())
}

/// The overlay, over everything: the window's last child, and painted after the parts that
/// are themselves painted late (the chat's working line, menus), with its dimmed backdrop.
pub fn render(cx: &App) -> Option<AnyElement> {
    let path = cx.try_global::<Shown>()?.0.clone()?;
    Some(deferred(
        div()
            .id("image-view")
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui_kit::black().opacity(0.82))
            .flex()
            .items_center()
            .justify_center()
            .p(px(56.))
            .cursor_pointer()
            .on_click(|_, _, cx| close(cx))
            .child(img(path).size_full().object_fit(ObjectFit::Contain))
            .child(
                div()
                    .id("image-view-close")
                    .absolute()
                    .top(px(14.))
                    .right(px(14.))
                    .h(px(34.))
                    .px(px(14.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded_full()
                    .bg(gpui_kit::white().opacity(0.92))
                    .text_color(gpui_kit::black())
                    .text_size(px(14.))
                    .font_weight(FontWeight::MEDIUM)
                    .shadow_md()
                    .hover(|d| d.bg(gpui_kit::white()))
                    .child(Icon::new(IconName::Close).small())
                    .child("Close")
                    .on_click(|_, _, cx| {
                        cx.stop_propagation();
                        close(cx)
                    }),
            )
            .child(
                div()
                    .absolute()
                    .bottom(px(18.))
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .text_size(px(12.))
                    .text_color(gpui_kit::white().opacity(0.7))
                    .child("Esc, or a click anywhere, closes it"),
            ),
    )
    // Above every other late part (menus use 1 and 2).
    .with_priority(100)
    .into_any_element())
}
