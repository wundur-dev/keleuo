//! A game asset in a tab (see `asset_store` and `keleuo_core::assets`): its history, larger
//! than its card in the chat, and later its model in the 3D viewer. The deciding happens in
//! the chat's card; this shows every try of every step, what was approved and what each cost.

use std::path::{Path, PathBuf};

use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent, PanelInfo, PanelState};
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleuo_core::assets::{self, Attempt, View};

use crate::asset_store;

/// What the workspace hears from an asset tab.
pub enum AssetEvent {
    Activated,
    /// Show a page (the 3D viewer's) in a web tab.
    OpenPage(PathBuf),
    Closed,
}

impl EventEmitter<AssetEvent> for AssetPanel {}
impl EventEmitter<PanelEvent> for AssetPanel {}

pub struct AssetPanel {
    pub dir: PathBuf,
    focus: FocusHandle,
}

impl AssetPanel {
    pub fn new(dir: PathBuf, cx: &mut Context<Self>) -> Self {
        Self { dir, focus: cx.focus_handle() }
    }

    fn section(&self, title: &str, tries: &[&Attempt], approved: &[&str], cx: &App) -> Div {
        let theme = cx.theme();
        let (muted, green, ring, border, field) = (theme.muted_foreground, theme.green, theme.ring, theme.border, theme.secondary);
        let mut row = div().flex().flex_wrap().gap_3();
        for (i, t) in tries.iter().enumerate() {
            let path = self.dir.join(&t.file);
            let full = path.clone();
            let ok = approved.contains(&t.file.as_str());
            row = row.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .w(px(180.))
                    .child(
                        div()
                            .id(SharedString::from(format!("hist-{title}-{i}")))
                            .size(px(180.))
                            .rounded_md()
                            .overflow_hidden()
                            .border_2()
                            .border_color(if ok { ring } else { border })
                            .bg(field)
                            .cursor_pointer()
                            .child(img(path).size_full().object_fit(ObjectFit::Contain))
                            .on_click(move |_, _, cx| crate::image_view::show(full.clone(), cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .text_xs()
                            .child(div().text_color(muted).child(format!("≈ {}", money(t.cents))))
                            .when(ok, |d| d.child(div().text_color(green).child("✓ Approved"))),
                    )
                    .child(div().text_xs().text_color(muted).line_clamp(2).child(t.asked.clone())),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().font_weight(FontWeight::SEMIBOLD).child(title.to_string()))
            .child(if tries.is_empty() { div().text_sm().text_color(muted).child("Nothing yet.") } else { row })
    }
}

impl AssetPanel {
    /// The models: each try's preview, with View in 3D (the viewer in a web tab beside it).
    fn models(&self, models: &[Attempt], approved: Option<&str>, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (muted, green, border) = (theme.muted_foreground, theme.green, theme.border);
        let mut row = div().flex().flex_wrap().gap_3();
        for (i, m) in models.iter().enumerate() {
            let (dir, file) = (self.dir.clone(), m.file.clone());
            let ok = approved == Some(m.file.as_str());
            row = row.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .w(px(180.))
                    .child(match &m.preview {
                        Some(p) => div().size(px(180.)).rounded_md().overflow_hidden().border_1().border_color(border).child(img(self.dir.join(p)).size_full().object_fit(ObjectFit::Contain)),
                        None => div().size(px(180.)).rounded_md().border_1().border_color(border),
                    })
                    .child(div().flex().gap_2().text_xs().child(div().text_color(muted).child(format!("≈ {}", money(m.cents)))).when(ok, |d| d.child(div().text_color(green).child("✓ Approved"))))
                    .child(div().flex().child(
                        gpui_kit::component::button::Button::new(SharedString::from(format!("hist-3d-{i}")))
                            .xsmall()
                            .label("View in 3D")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                let me = cx.entity().downgrade();
                                asset_store::view_3d(&dir, &file, cx, move |page, cx| {
                                    let _ = me.update(cx, |_, cx| cx.emit(AssetEvent::OpenPage(page)));
                                });
                            })),
                    )),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Models"))
            .child(if models.is_empty() { div().text_sm().text_color(muted).child("Nothing yet.") } else { row })
    }
}

fn money(cents: f32) -> String {
    let cents = cents.max(0.0) + 0.0;
    if cents < 100.0 { format!("{cents:.0}¢") } else { format!("${:.2}", cents / 100.0) }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

impl Focusable for AssetPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AssetPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entry = asset_store::entry(&self.dir, cx);
        let theme = cx.theme();
        let (bg, fg, muted) = (theme.background, theme.foreground, theme.muted_foreground);
        let root = div().id("asset-panel").track_focus(&self.focus).size_full().bg(bg).text_color(fg).overflow_y_scroll().p_4().flex().flex_col().gap_4();
        let Some(e) = entry else {
            return root.child(div().text_sm().text_color(muted).child(format!("No asset at {}", self.dir.display())));
        };
        let a = &e.asset;
        let concepts: Vec<&Attempt> = a.concepts.iter().collect();
        let approved_concept: Vec<&str> = a.concept.iter().map(String::as_str).collect();
        let views: Vec<&Attempt> = View::ALL.iter().flat_map(|v| a.views.iter().filter(move |t| t.view == Some(*v))).collect();
        let approved_views: Vec<&str> = a.approved_views.iter().map(|(_, f)| f.as_str()).collect();
        root.child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(a.name.clone()))
                .child(div().text_sm().text_color(muted).child(format!("{} · {}", capitalize(&a.kind), a.stage.label())))
                .child(div().flex_1())
                .child(div().text_sm().text_color(muted).child(format!("≈ {} so far", money(a.cents())))),
        )
        .child(div().text_sm().text_color(muted).child(a.brief.clone()))
        .child(div().text_xs().text_color(muted).child("Decide in the asset's card in the chat. Click a picture to see it full size."))
        .child(self.section("Concepts", &concepts, &approved_concept, cx))
        .child(self.section("Views", &views, &approved_views, cx))
        .child(self.models(&a.models, a.model.as_deref(), cx))
    }
}

impl BasePanel for AssetPanel {
    fn panel_name(&self) -> &'static str {
        "Asset"
    }
    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(AssetEvent::Closed);
    }
    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        if active {
            cx.emit(AssetEvent::Activated);
        }
    }
    fn dump(&self, _: &App) -> PanelState {
        let mut s = PanelState::new(self.panel_name());
        s.info = PanelInfo::panel(serde_json::json!({ "dir": self.dir }));
        s
    }
}

impl Panel for AssetPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = asset_store::entry(&self.dir, cx).map(|e| e.asset.name).unwrap_or_else(|| "Asset".into());
        div().flex().items_center().gap_1().child(Icon::new(gpui_kit::assets::IconName::Box).xsmall()).child(div().max_w(px(180.)).truncate().child(name))
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

/// Whether `path` is an asset's record (opening it opens the asset).
pub fn is_record(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == assets::RECORD)
}
