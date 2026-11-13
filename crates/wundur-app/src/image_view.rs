//! An image shown over the whole window, as large as it fits: a picture in the chat (a pasted
//! one, one sent with a message, a screenshot a tool looked at) opened full size. Copy (or
//! Cmd+C) puts the image itself on the clipboard; a Close button, Esc, or a click anywhere
//! closes it.

use std::path::PathBuf;

use gpui_kit::component::{Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;

/// The image shown, if any, and whether it was copied (the button says so).
#[derive(Default)]
struct Shown(Option<PathBuf>, bool);
impl Global for Shown {}

pub fn show(path: PathBuf, cx: &mut App) {
    cx.set_global(Shown(Some(path), false));
    cx.refresh_windows();
}

pub fn close(cx: &mut App) {
    cx.set_global(Shown(None, false));
    cx.refresh_windows();
}

pub fn is_open(cx: &App) -> bool {
    cx.try_global::<Shown>().is_some_and(|s| s.0.is_some())
}

/// The format of an image file, by its first bytes (then its extension).
fn format_of(bytes: &[u8], path: &std::path::Path) -> ImageFormat {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => ImageFormat::Png,
        [0xFF, 0xD8, ..] => ImageFormat::Jpeg,
        [b'G', b'I', b'F', ..] => ImageFormat::Gif,
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => ImageFormat::Webp,
        [b'B', b'M', ..] => ImageFormat::Bmp,
        _ => match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
            Some("jpg" | "jpeg") => ImageFormat::Jpeg,
            Some("gif") => ImageFormat::Gif,
            Some("webp") => ImageFormat::Webp,
            Some("svg") => ImageFormat::Svg,
            _ => ImageFormat::Png,
        },
    }
}

/// Put the shown image on the clipboard (the image, not its path): pasted into another app,
/// it comes out as a picture. True when there was one to copy.
pub fn copy(cx: &mut App) -> bool {
    let Some(path) = cx.try_global::<Shown>().and_then(|s| s.0.clone()) else { return false };
    let Ok(bytes) = std::fs::read(&path) else { return false };
    let format = format_of(&bytes, &path);
    cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(format, bytes)));
    cx.set_global(Shown(Some(path), true));
    cx.refresh_windows();
    true
}

/// The overlay, over everything: the window's last child, and painted after the parts that
/// are themselves painted late (the chat's working line, menus), with its dimmed backdrop.
pub fn render(cx: &App) -> Option<AnyElement> {
    let shown = cx.try_global::<Shown>()?;
    let (path, copied) = (shown.0.clone()?, shown.1);
    // A pill-shaped button over the image's top right.
    let pill = |id: &'static str| {
        div()
            .id(id)
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
    };
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
                    .absolute()
                    .top(px(14.))
                    .right(px(14.))
                    .flex()
                    .gap(px(8.))
                    .child(
                        pill("image-view-copy")
                            .child(Icon::new(if copied { IconName::Check } else { IconName::Copy }).small())
                            .child(if copied { "Copied" } else { "Copy" })
                            .on_click(|_, _, cx| {
                                cx.stop_propagation();
                                copy(cx);
                            }),
                    )
                    .child(pill("image-view-close").child(Icon::new(IconName::Close).small()).child("Close").on_click(|_, _, cx| {
                        cx.stop_propagation();
                        close(cx)
                    })),
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
                    .child("Cmd+C copies it · Esc, or a click anywhere, closes it"),
            ),
    )
    // Above every other late part (menus use 1 and 2).
    .with_priority(100)
    .into_any_element())
}
