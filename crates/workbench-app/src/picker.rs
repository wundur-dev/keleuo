//! The folder picker: shown when Workbench starts without a folder, or with one that does not
//! exist. Recent folders are one click away; "Open Folder..." asks the system dialog.

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;

pub struct Picker {
    pub message: Option<String>,
    pub recent: Vec<PathBuf>,
}

impl Picker {
    /// Open `path` in a workspace window and close this one.
    pub fn choose(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        crate::open_workspace(
            std::sync::Arc::new(workbench_core::host::LocalHost),
            path,
            cx,
        );
        window.remove_window();
    }

    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update_in(cx, |p, window, cx| p.choose(path, window, cx));
            }
        })
        .detach();
    }
}

impl Render for Picker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, fg, muted, hover, warning, border) = (
            theme.background,
            theme.foreground,
            theme.muted_foreground,
            theme.secondary_hover,
            theme.warning,
            theme.border,
        );
        let mut list = div().flex().flex_col().gap_px();
        if self.recent.is_empty() {
            list = list.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("No recent folders yet."),
            );
        }
        for (i, path) in self.recent.iter().enumerate() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let parent = path
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let target = path.clone();
            list = list.child(
                div()
                    .id(("recent", i))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .flex()
                    .items_center()
                    .gap_2()
                    .hover(|d| d.bg(hover))
                    .cursor_pointer()
                    .child(Icon::new(IconName::Folder).small().text_color(muted))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(div().text_sm().text_color(fg).child(name))
                            .child(div().text_xs().text_color(muted).truncate().child(parent)),
                    )
                    .on_click(
                        cx.listener(move |p, _, window, cx| p.choose(target.clone(), window, cx)),
                    ),
            );
        }
        div()
            .size_full()
            .bg(bg)
            .flex()
            .flex_col()
            .pt(px(40.))
            .px_6()
            .gap_3()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(fg)
                    .child("Open a folder"),
            )
            .when_some(self.message.clone(), |d, m| {
                d.child(div().text_sm().text_color(warning).child(m))
            })
            .child(div().text_xs().text_color(muted).child("RECENT"))
            .child(div().border_t_1().border_color(border).pt_1().child(list))
            .child(
                Button::new("browse")
                    .primary()
                    .small()
                    .label("Open Folder...")
                    .on_click(cx.listener(|p, _, window, cx| p.browse(window, cx))),
            )
    }
}
