//! A game asset's card in the chat (see `asset_store` and `keleuo_core::assets`): its current
//! step, where the person decides. Concept: the tries, one picked, changed with a note, then
//! approved. Views: front, side and back, each made, tried again and approved. The asset's tab
//! (Open) shows the same asset larger, with its history and the 3D viewer. Also the form that
//! starts one, a card of its own until it's filled in.

use std::path::{Path, PathBuf};

use gpui_kit::component::Disableable;
use gpui_kit::component::input::{Input, InputState};
use keleuo_core::assets::{Make, Stage, View};

use super::*;
use crate::asset_store;

/// The new-asset form's fields.
pub(super) struct NewAssetForm {
    name: Entity<InputState>,
    brief: Entity<InputState>,
    kind: &'static str,
    error: Option<String>,
}

const KINDS: [&str; 4] = ["character", "creature", "prop", "environment"];

fn money(cents: f32) -> String {
    let cents = cents.max(0.0) + 0.0;
    if cents < 100.0 { format!("{cents:.0}¢") } else { format!("${:.2}", cents / 100.0) }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

impl ChatPanel {
    /// Show the form for a new asset at the end of the chat.
    pub fn start_asset_form(&mut self, cx: &mut Context<Self>) {
        self.session.update(cx, |s, cx| {
            if !s.chat.items.iter().any(|i| matches!(i, Item::NewAsset)) {
                s.chat.items.push(Item::NewAsset);
                s.chat.revision += 1;
            }
            cx.notify();
        });
    }

    /// Test commands, as the card's buttons, for this chat's latest asset: `new <name>|<brief>`
    /// (the form, filled in and created), `concept`, `change <note>`, `approve`, `views`,
    /// `view <front|side|back>`, `approve-view <front|side|back>`, `model`, `approve-model`,
    /// `view-3d`, `state`.
    pub fn asset_command(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
        if cmd == "new" {
            self.start_asset_form(cx);
            self.ensure_asset_fields(window, cx);
            let (name, brief) = arg.split_once('|').unwrap_or((arg, arg));
            if let Some(f) = &self.new_asset {
                let (n, b) = (f.name.clone(), f.brief.clone());
                n.update(cx, |i, cx| i.set_value(name.to_string(), window, cx));
                b.update(cx, |i, cx| i.set_value(brief.to_string(), window, cx));
            }
            self.create_asset(cx);
        }
        let Some(dir) = self.session.read(cx).chat.items.iter().rev().find_map(|i| if let Item::Asset(d) = i { Some(d.clone()) } else { None }) else {
            return "no asset in this chat".into();
        };
        let view = |arg: &str| View::ALL.into_iter().find(|v| v.label().eq_ignore_ascii_case(arg));
        match cmd {
            "new" | "state" => {}
            "concept" => asset_store::make(&dir, Make::Concept { prompt: String::new() }, "a concept".into(), cx, |_, _| {}),
            "change" => {
                if let Some(from) = asset_store::entry(&dir, cx).and_then(|e| e.picked) {
                    asset_store::make(&dir, Make::Change { from, note: arg.to_string() }, "the change".into(), cx, |_, _| {});
                }
            }
            "approve" => asset_store::approve_concept(&dir, cx),
            "model" => asset_store::make(&dir, Make::Model, "the model".into(), cx, |_, _| {}),
            "approve-model" => {
                if let Some(f) = asset_store::entry(&dir, cx).and_then(|e| e.asset.models.last().map(|m| m.file.clone())) {
                    asset_store::approve_model(&dir, &f, cx);
                }
            }
            "view-3d" => {
                let me = cx.entity().downgrade();
                if let Some(f) = asset_store::entry(&dir, cx).and_then(|e| e.asset.models.last().map(|m| m.file.clone())) {
                    asset_store::view_3d(&dir, &f, cx, move |page, cx| {
                        let _ = me.update(cx, |_, cx| cx.emit(ChatPanelEvent::OpenWeb(keleuo_core::web::file_url(&page))));
                    });
                }
            }
            "views" => asset_store::make_views(&dir, cx),
            "view" => {
                if let Some(v) = view(arg) {
                    asset_store::make(&dir, Make::View(v), format!("the {arg} view"), cx, |_, _| {});
                }
            }
            "approve-view" => {
                if let Some(v) = view(arg)
                    && let Some(f) = asset_store::entry(&dir, cx).and_then(|e| e.asset.views.iter().rev().find(|t| t.view == Some(v)).map(|t| t.file.clone()))
                {
                    asset_store::approve_view(&dir, v, &f, cx);
                }
            }
            _ => return format!("unknown asset command: {cmd}"),
        }
        let Some(e) = asset_store::entry(&dir, cx) else { return "gone".into() };
        format!(
            "{}: stage {:?}, {} concepts, {} views, {} models, picked {:?}, busy {:?}, error {:?}, {}",
            e.asset.name,
            e.asset.stage,
            e.asset.concepts.len(),
            e.asset.views.len(),
            e.asset.models.len(),
            e.picked,
            e.busy,
            e.error,
            money(e.asset.cents())
        )
    }

    /// Before drawing: the fields the asset cards need (made with the window).
    pub(super) fn ensure_asset_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (dirs, form): (Vec<PathBuf>, bool) = {
            let chat = &self.session.read(cx).chat;
            (
                chat.items.iter().filter_map(|i| if let Item::Asset(d) = i { Some(d.clone()) } else { None }).collect(),
                chat.items.iter().any(|i| matches!(i, Item::NewAsset)),
            )
        };
        for dir in dirs {
            self.asset_notes
                .entry(dir)
                .or_insert_with(|| cx.new(|cx| InputState::new(window, cx).placeholder("What to change, e.g. a blue cape, a shorter sword")));
        }
        if form && self.new_asset.is_none() {
            self.new_asset = Some(NewAssetForm {
                name: cx.new(|cx| InputState::new(window, cx).placeholder("Name, e.g. Knight")),
                brief: cx.new(|cx| InputState::new(window, cx).placeholder("What it is: look, colours, style, clothes or materials")),
                kind: "character",
                error: None,
            });
        }
        // The asset store changed (a try came back, a step approved): the cards' heights did.
        let revision = asset_store::revision(cx);
        if revision != self.asset_revision {
            self.asset_revision = revision;
            self.list.remeasure();
        }
    }

    fn close_asset_form(&mut self, cx: &mut Context<Self>, replace: Option<PathBuf>) {
        self.new_asset = None;
        self.session.update(cx, |s, cx| {
            if let Some(at) = s.chat.items.iter().position(|i| matches!(i, Item::NewAsset)) {
                match replace {
                    Some(dir) => s.chat.items[at] = Item::Asset(dir),
                    None => {
                        s.chat.items.remove(at);
                    }
                }
                s.chat.revision += 1;
            }
            cx.notify();
        });
    }

    fn create_asset(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &self.new_asset else { return };
        let (name, brief, kind) = (form.name.read(cx).value().to_string(), form.brief.read(cx).value().to_string(), form.kind);
        let (project, origin) = {
            let s = self.session.read(cx);
            let chat = &s.chat;
            let turn = chat.items.iter().rposition(|i| matches!(i, Item::User(_))).and_then(|from| keleuo_core::summary::TurnText::key_at(chat, from));
            (s.cwd().to_path_buf(), chat.session_id.clone().map(|session| keleuo_core::assets::Origin { session, turn }))
        };
        match asset_store::create(&project, &name, kind, &brief, origin, cx) {
            Ok(dir) => self.close_asset_form(cx, Some(dir)),
            Err(e) => {
                if let Some(f) = &mut self.new_asset {
                    f.error = Some(e);
                }
                cx.notify();
            }
        }
    }

    /// The card's frame: a header (what it is, the step, the cost, Open) and the body.
    fn asset_frame(&self, title: AnyElement, right: Option<AnyElement>, body: AnyElement, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let (border, accent) = (theme.border, theme.cyan);
        div()
            .flex()
            .flex_col()
            .border_1()
            .border_color(border)
            .rounded_md()
            .bg(accent.opacity(0.04))
            .overflow_hidden()
            .child(
                div()
                    .h(px(28.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .child(Icon::new(gpui_kit::assets::IconName::Box).xsmall().text_color(accent))
                    .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(accent).child("asset"))
                    .child(title)
                    .child(div().flex_1())
                    .children(right),
            )
            .child(div().p_2().border_t_1().border_color(border).child(body))
            .into_any_element()
    }

    pub(super) fn render_new_asset(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, danger) = (theme.foreground, theme.muted_foreground, theme.danger);
        let Some(form) = &self.new_asset else { return div().into_any_element() };
        let mut kinds = div().flex().gap_1();
        for k in KINDS {
            let on = form.kind == k;
            kinds = kinds.child(
                Button::new(SharedString::from(format!("new-asset-kind-{k}")))
                    .xsmall()
                    .label(capitalize(k))
                    .when(on, |b| b.primary())
                    .when(!on, |b| b.outline())
                    .on_click(cx.listener(move |p, _, _, cx| {
                        if let Some(f) = &mut p.new_asset {
                            f.kind = k;
                        }
                        cx.notify();
                    })),
            );
        }
        let body = div()
            .flex()
            .flex_col()
            .gap_2()
            .text_sm()
            .text_color(fg)
            .child(div().text_xs().text_color(muted).child("Made step by step, you approving each: a concept (cheap tries until it's right), then front, side and back views, then the 3D model and its rig."))
            .child(Input::new(&form.name).small())
            .child(Input::new(&form.brief).small())
            .child(kinds)
            .children(form.error.clone().map(|e| div().text_xs().text_color(danger).child(e)))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(Button::new("new-asset-create").xsmall().primary().label("Create").on_click(cx.listener(|p, _, _, cx| p.create_asset(cx))))
                    .child(Button::new("new-asset-cancel").xsmall().ghost().label("Dismiss").on_click(cx.listener(|p, _, _, cx| p.close_asset_form(cx, None)))),
            )
            .into_any_element();
        self.asset_frame(div().text_color(fg).child("New game asset").into_any_element(), None, body, cx)
    }

    /// A thumbnail of a file of the asset in `dir`; double-click: full size.
    fn asset_thumb(&self, id: SharedString, dir: &Path, file: &str, size: f32, picked: bool, cx: &App) -> Stateful<Div> {
        let theme = cx.theme();
        let path = dir.join(file);
        let full = path.clone();
        div()
            .id(id)
            .flex_none()
            .size(px(size))
            .rounded_md()
            .overflow_hidden()
            .border_2()
            .border_color(if picked { theme.ring } else { theme.border })
            .bg(theme.secondary)
            .cursor_pointer()
            .child(img(path).size_full().object_fit(ObjectFit::Contain))
            .on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, _, cx| {
                if e.click_count >= 2 {
                    crate::image_view::show(full.clone(), cx);
                }
            })
    }

    pub(super) fn render_asset_card(&self, dir: &Path, cx: &mut Context<Self>) -> AnyElement {
        let entry = asset_store::entry(dir, cx);
        let theme = cx.theme();
        let (fg, muted, danger, green, border) = (theme.foreground, theme.muted_foreground, theme.danger, theme.green, theme.border);
        let Some(e) = entry else {
            return self.asset_frame(div().text_color(muted).child(format!("{} (gone)", dir.display())).into_any_element(), None, div().into_any_element(), cx);
        };
        let a = &e.asset;
        let busy = e.busy.is_some();
        let key = dir.to_string_lossy().to_string();
        let id = |what: &str| SharedString::from(format!("asset-{what}-{key}"));
        let open_dir = dir.to_path_buf();
        let title = div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(a.name.clone()))
            .child(div().text_color(muted).child(format!("· {}", a.stage.label())))
            .into_any_element();
        let right = div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().text_color(muted).child(format!("≈ {}", money(a.cents()))))
            .child(Button::new(id("open")).xsmall().ghost().label("Open").on_click(cx.listener(move |_, _, _, cx| cx.emit(ChatPanelEvent::OpenAsset(open_dir.clone())))))
            .into_any_element();

        let mut body = div().flex().flex_col().gap_2().text_sm().text_color(fg);
        if let Some(what) = &e.busy {
            body = body.child(div().flex().items_center().gap_2().text_xs().text_color(muted).child(Icon::new(IconName::Loader).xsmall()).child(format!("Making {what}…")));
        }
        if let Some(err) = &e.error {
            body = body.child(div().text_xs().text_color(danger).child(err.clone()));
        }
        match a.stage {
            Stage::Concept => {
                let mut tries = div().flex().flex_wrap().gap_2();
                for (i, t) in a.concepts.iter().enumerate() {
                    let (d, f) = (dir.to_path_buf(), t.file.clone());
                    tries = tries.child(
                        self.asset_thumb(id(&format!("c{i}")), dir, &t.file, 96., e.picked.as_deref() == Some(t.file.as_str()), cx)
                            .on_click(move |_, _, cx| asset_store::pick(&d, &f, cx)),
                    );
                }
                let d = dir.to_path_buf();
                let make = Button::new(id("make")).xsmall().label(if a.concepts.is_empty() { "Make a concept" } else { "Make another" }).disabled(busy);
                let make = if a.concepts.is_empty() { make.primary() } else { make.outline() };
                body = body
                    .child(div().text_xs().text_color(muted).child(a.brief.clone()))
                    .when(!a.concepts.is_empty(), |b| b.child(tries))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(make.on_click(move |_, _, cx| asset_store::make(&d, Make::Concept { prompt: String::new() }, "a concept".into(), cx, |_, _| {})))
                            .child(div().text_xs().text_color(muted).child("about 1¢ each")),
                    );
                if !a.concepts.is_empty()
                    && let Some(note) = self.asset_notes.get(dir)
                {
                    let (d1, d2, n) = (dir.to_path_buf(), dir.to_path_buf(), note.clone());
                    let picked = e.picked.clone();
                    body = body
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().flex_1().child(Input::new(note).small()))
                                .child(Button::new(id("change")).xsmall().outline().label("Change it").disabled(busy || picked.is_none()).on_click(move |_, window, cx| {
                                    let text = n.read(cx).value().trim().to_string();
                                    if text.is_empty() {
                                        return;
                                    }
                                    if let Some(from) = picked.clone() {
                                        n.update(cx, |i, cx| i.set_value("", window, cx));
                                        asset_store::make(&d1, Make::Change { from, note: text }, "the change".into(), cx, |_, _| {});
                                    }
                                })),
                        )
                        .child(div().flex().items_center().gap_2().child(
                            Button::new(id("approve")).xsmall().primary().icon(IconName::Check).label("Approve the picked one").disabled(busy || e.picked.is_none()).on_click(move |_, _, cx| asset_store::approve_concept(&d2, cx)),
                        ).child(div().text_xs().text_color(muted).child("Click a try to pick it; double-click to see it full size.")));
                }
            }
            Stage::Views => {
                let mut slots = div().flex().flex_wrap().gap_3();
                for v in View::ALL {
                    let tries: Vec<_> = a.views.iter().filter(|t| t.view == Some(v)).collect();
                    let approved = a.approved_view(v).map(str::to_string);
                    let shown = approved.clone().or_else(|| tries.last().map(|t| t.file.clone()));
                    let mut slot = div().flex().flex_col().gap_1().child(div().text_xs().font_weight(FontWeight::MEDIUM).child(v.label()));
                    slot = match &shown {
                        Some(f) => slot.child(self.asset_thumb(id(&format!("v-{}", v.label())), dir, f, 112., approved.is_some(), cx)),
                        None => slot.child(div().size(px(112.)).rounded_md().border_1().border_color(border).flex().items_center().justify_center().text_xs().text_color(muted).child("Not made yet")),
                    };
                    let (d1, d2, f) = (dir.to_path_buf(), dir.to_path_buf(), shown.clone());
                    slot = slot.child(
                        div()
                            .flex()
                            .gap_1()
                            .child(Button::new(id(&format!("vm-{}", v.label()))).xsmall().outline().label(if tries.is_empty() { "Make" } else { "Again" }).disabled(busy).on_click(move |_, _, cx| {
                                asset_store::make(&d1, Make::View(v), format!("the {} view", v.label().to_lowercase()), cx, |_, _| {})
                            }))
                            .when(approved.is_none() && f.is_some(), |d| {
                                d.child(Button::new(id(&format!("va-{}", v.label()))).xsmall().primary().label("Approve").on_click(move |_, _, cx| {
                                    if let Some(f) = &f {
                                        asset_store::approve_view(&d2, v, f, cx);
                                    }
                                }))
                            })
                            .when(approved.is_some(), |d| d.child(div().text_xs().text_color(green).child("✓"))),
                    );
                    slots = slots.child(slot);
                }
                let d = dir.to_path_buf();
                body = body
                    .child(div().flex().items_center().gap_2().children(a.concept.clone().map(|c| self.asset_thumb(id("concept"), dir, &c, 40., false, cx))).child(div().text_xs().text_color(muted).child(
                        "Front, side and back from the approved concept, for the 3D model. About 10¢ each.",
                    )))
                    .when(View::ALL.iter().any(|v| !a.views.iter().any(|t| t.view == Some(*v))), |b| {
                        b.child(div().flex().child(Button::new(id("views")).xsmall().primary().label("Make the missing views").disabled(busy).on_click(move |_, _, cx| asset_store::make_views(&d, cx))))
                    })
                    .child(slots);
            }
            Stage::Model => {
                let me = cx.entity().downgrade();
                let mut tries = div().flex().flex_wrap().gap_3();
                for (i, t) in a.models.iter().enumerate() {
                    let (d1, d2, f1, f2, me) = (dir.to_path_buf(), dir.to_path_buf(), t.file.clone(), t.file.clone(), me.clone());
                    let thumb = match &t.preview {
                        Some(p) => self.asset_thumb(id(&format!("m{i}")), dir, p, 128., false, cx).into_any_element(),
                        None => div().size(px(128.)).rounded_md().border_1().border_color(border).flex().items_center().justify_center().text_xs().text_color(muted).child("No preview").into_any_element(),
                    };
                    tries = tries.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(thumb)
                            .child(div().text_xs().text_color(muted).child(format!("≈ {}", money(t.cents))))
                            .child(
                                div()
                                    .flex()
                                    .gap_1()
                                    .child(Button::new(id(&format!("m3d{i}"))).xsmall().outline().label("View in 3D").on_click(move |_, _, cx| {
                                        let me = me.clone();
                                        asset_store::view_3d(&d1, &f1, cx, move |page, cx| {
                                            let _ = me.update(cx, |_, cx| cx.emit(ChatPanelEvent::OpenWeb(keleuo_core::web::file_url(&page))));
                                        });
                                    }))
                                    .child(Button::new(id(&format!("ma{i}"))).xsmall().primary().label("Approve").disabled(busy).on_click(move |_, _, cx| asset_store::approve_model(&d2, &f2, cx))),
                            ),
                    );
                }
                let d = dir.to_path_buf();
                body = body
                    .child(div().text_xs().text_color(muted).child("A textured 3D model from the three approved views, by Tripo (a minute or a few). About 30¢, or $1 with Tripo's P models."))
                    .when(!a.models.is_empty(), |b| b.child(tries))
                    .child(div().flex().child(
                        Button::new(id("model"))
                            .xsmall()
                            .label(if a.models.is_empty() { "Make the model" } else { "Try again" })
                            .when(a.models.is_empty(), |b| b.primary())
                            .when(!a.models.is_empty(), |b| b.outline())
                            .disabled(busy)
                            .on_click(move |_, _, cx| asset_store::make(&d, Make::Model, "the model (a minute or a few)".into(), cx, |_, _| {})),
                    ));
            }
            _ => {
                let me = cx.entity().downgrade();
                let (d, f) = (dir.to_path_buf(), a.model.clone().unwrap_or_default());
                body = body
                    .child(div().text_xs().text_color(muted).child("The model is approved. Rigging comes next: Mixamo in the web panel for a character, Tripo's rig for a creature."))
                    .child(div().flex().child(Button::new(id("view-approved")).xsmall().outline().label("View in 3D").on_click(move |_, _, cx| {
                        let me = me.clone();
                        asset_store::view_3d(&d, &f, cx, move |page, cx| {
                            let _ = me.update(cx, |_, cx| cx.emit(ChatPanelEvent::OpenWeb(keleuo_core::web::file_url(&page))));
                        });
                    })));
            }
        }
        self.asset_frame(title, Some(right), body.into_any_element(), cx)
    }
}
