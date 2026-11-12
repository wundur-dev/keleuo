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
            "concept" => asset_store::make(&dir, Make::Concept { prompt: String::new() }, cx, |_, _| {}),
            "change" => {
                if let Some(from) = asset_store::entry(&dir, cx).and_then(|e| e.picked) {
                    asset_store::make(&dir, Make::Change { from, note: arg.to_string() }, cx, |_, _| {});
                }
            }
            "approve" => asset_store::approve_concept(&dir, cx),
            "model" => asset_store::make(&dir, Make::Model, cx, |_, _| {}),
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
                    asset_store::make(&dir, Make::View(v), cx, |_, _| {});
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
            "{}: stage {:?}, {} concepts, {} views, {} models, picked {:?}, busy {:?}, error {:?}",
            e.asset.name,
            e.asset.stage,
            e.asset.concepts.len(),
            e.asset.views.len(),
            e.asset.models.len(),
            e.picked,
            e.busy,
            e.error,
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

    /// The card: a quiet header (its name and kind, how far it has got, Open) and the step.
    #[allow(clippy::too_many_arguments)]
    fn asset_frame(&self, id: SharedString, name: String, kind: Option<String>, stage: Option<Stage>, open: Option<PathBuf>, body: AnyElement, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, bg) = (theme.foreground, theme.muted_foreground, theme.border, theme.background);
        div()
            .flex()
            .flex_col()
            .rounded(px(14.))
            .border_1()
            .border_color(border.opacity(0.8))
            .bg(bg)
            .shadow_sm()
            .overflow_hidden()
            .child(
                div()
                    .px_3()
                    .pt_3()
                    .pb_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .size(px(26.))
                            .flex_none()
                            .rounded(px(8.))
                            .bg(theme.secondary)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(gpui_kit::assets::IconName::Box).small().text_color(muted)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(name))
                            .children(kind.map(|k| div().text_xs().text_color(muted).child(k))),
                    )
                    .child(div().flex_1())
                    .children(stage.map(|s| stepper(s, cx)))
                    .children(open.map(|dir| {
                        Button::new(SharedString::from(format!("{id}-open")))
                            .xsmall()
                            .ghost()
                            .icon(gpui_kit::assets::IconName::Maximize2)
                            .tooltip("Open the asset")
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(ChatPanelEvent::OpenAsset(dir.clone()))))
                            .into_any_element()
                    })),
            )
            .child(div().px_3().pb_3().child(body))
            .into_any_element()
    }

    pub(super) fn render_new_asset(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let danger = theme.danger;
        let Some(form) = &self.new_asset else { return div().into_any_element() };
        let mut kinds = div().flex().gap_1();
        for k in KINDS {
            let on = form.kind == k;
            kinds = kinds.child(
                Button::new(SharedString::from(format!("new-asset-kind-{k}")))
                    .xsmall()
                    .label(capitalize(k))
                    .when(on, |b| b.primary())
                    .when(!on, |b| b.ghost())
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
            .child(Input::new(&form.name))
            .child(Input::new(&form.brief))
            .children(form.error.clone().map(|e| div().text_xs().text_color(danger).child(e)))
            .child(
                div()
                    .pt_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(kinds)
                    .child(div().flex_1())
                    .child(Button::new("new-asset-cancel").small().ghost().label("Cancel").on_click(cx.listener(|p, _, _, cx| p.close_asset_form(cx, None))))
                    .child(Button::new("new-asset-create").small().primary().label("Create").on_click(cx.listener(|p, _, _, cx| p.create_asset(cx)))),
            )
            .into_any_element();
        self.asset_frame("new-asset".into(), "New asset".into(), None, None, None, body, cx)
    }

    /// A picture of the asset in `dir`: rounded, a ring and a check when chosen; double-click
    /// shows it full size.
    fn asset_tile(&self, id: SharedString, dir: &Path, file: &str, size: f32, chosen: bool, cx: &App) -> Stateful<Div> {
        let theme = cx.theme();
        let path = dir.join(file);
        let full = path.clone();
        div()
            .id(id)
            .relative()
            .flex_none()
            .size(px(size))
            .rounded(px(12.))
            .overflow_hidden()
            .bg(theme.secondary)
            .border_2()
            .border_color(if chosen { theme.ring } else { transparent_black() })
            .cursor_pointer()
            .hover(|d| d.shadow_md())
            .child(img(path).size_full().object_fit(ObjectFit::Cover))
            .when(chosen, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(px(6.))
                        .right(px(6.))
                        .size(px(20.))
                        .rounded_full()
                        .bg(theme.ring)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::new(IconName::Check).xsmall().text_color(gpui_kit::white())),
                )
            })
            .on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, _, cx| {
                if e.click_count >= 2 {
                    crate::image_view::show(full.clone(), cx);
                }
            })
    }

    /// Where the next picture will be, while it's made.
    fn asset_making(&self, size: f32, label: Option<&str>, cx: &App) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        div()
            .relative()
            .flex_none()
            .size(px(size))
            .rounded(px(12.))
            .overflow_hidden()
            .child(gpui_kit::component::skeleton::Skeleton::new().size_full().rounded(px(12.)))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .child(gpui_kit::component::spinner::Spinner::new().color(muted))
                    .children(label.map(|l| div().text_xs().text_color(muted).child(l.to_string()))),
            )
            .into_any_element()
    }

    pub(super) fn render_asset_card(&self, dir: &Path, cx: &mut Context<Self>) -> AnyElement {
        let entry = asset_store::entry(dir, cx);
        let theme = cx.theme();
        let (muted, danger, green) = (theme.muted_foreground, theme.danger, theme.green);
        let key = dir.to_string_lossy().to_string();
        let id = |what: &str| SharedString::from(format!("asset-{what}-{key}"));
        let Some(e) = entry else {
            let body = div().text_xs().text_color(muted).child(dir.display().to_string()).into_any_element();
            return self.asset_frame(id("card"), "Asset not found".into(), None, None, None, body, cx);
        };
        let a = e.asset.clone();
        let job = |j: asset_store::Job| e.busy.contains(&j);
        let mut body = div().flex().flex_col().gap_3();
        match a.stage {
            Stage::Concept => {
                let making = job(asset_store::Job::Concept);
                let mut tiles = div().flex().flex_wrap().gap_2();
                for (i, t) in a.concepts.iter().enumerate() {
                    let (d, f) = (dir.to_path_buf(), t.file.clone());
                    tiles = tiles.child(
                        self.asset_tile(id(&format!("c{i}")), dir, &t.file, 104., e.picked.as_deref() == Some(t.file.as_str()), cx)
                            .on_click(move |_, _, cx| asset_store::pick(&d, &f, cx)),
                    );
                }
                if making {
                    tiles = tiles.child(self.asset_making(104., None, cx));
                }
                let d = dir.to_path_buf();
                if a.concepts.is_empty() && !making {
                    body = body.child(div().text_sm().text_color(muted).child(a.brief.clone())).child(
                        div().flex().justify_end().child(
                            Button::new(id("make"))
                                .small()
                                .primary()
                                .icon(gpui_kit::assets::IconName::Sparkles)
                                .label("Make a concept")
                                .on_click(move |_, _, cx| asset_store::make(&d, Make::Concept { prompt: String::new() }, cx, |_, _| {})),
                        ),
                    );
                } else {
                    body = body.child(tiles);
                    if let Some(note) = self.asset_notes.get(dir) {
                        let (d1, d2, d3, n) = (dir.to_path_buf(), dir.to_path_buf(), dir.to_path_buf(), note.clone());
                        let picked = e.picked.clone();
                        let change = move |window: &mut Window, cx: &mut App| {
                            let text = n.read(cx).value().trim().to_string();
                            if text.is_empty() {
                                return;
                            }
                            if let Some(from) = picked.clone() {
                                n.update(cx, |i, cx| i.set_value("", window, cx));
                                asset_store::make(&d1, Make::Change { from, note: text }, cx, |_, _| {});
                            }
                        };
                        body = body
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().flex_1().child(Input::new(note)))
                                    .child(Button::new(id("change")).small().outline().label("Change").disabled(making || e.picked.is_none()).on_click(move |_, window, cx| change(window, cx))),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(Button::new(id("another")).small().ghost().icon(gpui_kit::assets::IconName::RefreshCw).label("Another").disabled(making).on_click(move |_, _, cx| {
                                        asset_store::make(&d2, Make::Concept { prompt: String::new() }, cx, |_, _| {})
                                    }))
                                    .child(div().flex_1())
                                    .child(Button::new(id("approve")).small().primary().icon(IconName::Check).label("Use this one").disabled(making || e.picked.is_none()).on_click(move |_, _, cx| asset_store::approve_concept(&d3, cx))),
                            );
                    }
                }
            }
            Stage::Views => {
                let any_view = View::ALL.iter().any(|v| job(asset_store::Job::View(*v)));
                let mut row = div().flex().flex_wrap().gap_3();
                for v in View::ALL {
                    let tries: Vec<_> = a.views.iter().filter(|t| t.view == Some(v)).collect();
                    let approved = a.approved_view(v).map(str::to_string);
                    let shown = approved.clone().or_else(|| tries.last().map(|t| t.file.clone()));
                    let making = job(asset_store::Job::View(v));
                    let tile = if making {
                        self.asset_making(118., None, cx)
                    } else {
                        match &shown {
                            Some(f) => self.asset_tile(id(&format!("v-{}", v.label())), dir, f, 118., approved.is_some(), cx).into_any_element(),
                            None => div().size(px(118.)).rounded(px(12.)).bg(theme.secondary).into_any_element(),
                        }
                    };
                    let (d1, d2, f) = (dir.to_path_buf(), dir.to_path_buf(), shown.clone());
                    row = row.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(tile)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(div().flex_1().text_xs().text_color(muted).child(v.label()))
                                    .when(!tries.is_empty(), |d| {
                                        d.child(Button::new(id(&format!("vm-{}", v.label()))).xsmall().ghost().icon(gpui_kit::assets::IconName::RefreshCw).tooltip("Another").disabled(making).on_click(move |_, _, cx| {
                                            asset_store::make(&d1, Make::View(v), cx, |_, _| {})
                                        }))
                                    })
                                    .when(approved.is_none() && f.is_some(), |d| {
                                        d.child(Button::new(id(&format!("va-{}", v.label()))).xsmall().ghost().icon(IconName::Check).tooltip("Use this one").on_click(move |_, _, cx| {
                                            if let Some(f) = &f {
                                                asset_store::approve_view(&d2, v, f, cx);
                                            }
                                        }))
                                    })
                                    .when(approved.is_some(), |d| d.child(Icon::new(IconName::Check).xsmall().text_color(green))),
                            ),
                    );
                }
                let missing = View::ALL.iter().any(|v| !a.views.iter().any(|t| t.view == Some(*v)));
                let d = dir.to_path_buf();
                body = body.child(row).when(missing && !any_view, |b| {
                    b.child(div().flex().justify_end().child(
                        Button::new(id("views")).small().primary().icon(gpui_kit::assets::IconName::Sparkles).label("Make the views").on_click(move |_, _, cx| asset_store::make_views(&d, cx)),
                    ))
                });
            }
            Stage::Model => {
                let making = job(asset_store::Job::Model);
                let me = cx.entity().downgrade();
                let mut row = div().flex().flex_wrap().gap_3();
                for (i, t) in a.models.iter().enumerate() {
                    let (d1, d2, f1, f2, me) = (dir.to_path_buf(), dir.to_path_buf(), t.file.clone(), t.file.clone(), me.clone());
                    let tile = match &t.preview {
                        Some(p) => self.asset_tile(id(&format!("m{i}")), dir, p, 150., false, cx).into_any_element(),
                        None => div().size(px(150.)).rounded(px(12.)).bg(theme.secondary).into_any_element(),
                    };
                    row = row.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(tile)
                            .child(
                                div()
                                    .flex()
                                    .gap_1()
                                    .child(Button::new(id(&format!("m3d{i}"))).xsmall().ghost().icon(gpui_kit::assets::IconName::Box).label("3D").tooltip("View in 3D").on_click(move |_, _, cx| {
                                        let me = me.clone();
                                        asset_store::view_3d(&d1, &f1, cx, move |page, cx| {
                                            let _ = me.update(cx, |_, cx| cx.emit(ChatPanelEvent::OpenWeb(keleuo_core::web::file_url(&page))));
                                        });
                                    }))
                                    .child(div().flex_1())
                                    .child(Button::new(id(&format!("ma{i}"))).xsmall().primary().icon(IconName::Check).label("Use").disabled(making).on_click(move |_, _, cx| asset_store::approve_model(&d2, &f2, cx))),
                            ),
                    );
                }
                if making {
                    row = row.child(self.asset_making(150., Some("A minute or a few"), cx));
                }
                let d = dir.to_path_buf();
                let first = a.models.is_empty();
                body = body
                    .when(!first || making, |b| b.child(row))
                    .when(!making, |b| {
                        b.child(div().flex().justify_end().child(
                            Button::new(id("model"))
                                .small()
                                .when(first, |b| b.primary().icon(gpui_kit::assets::IconName::Sparkles).label("Make the model"))
                                .when(!first, |b| b.ghost().icon(gpui_kit::assets::IconName::RefreshCw).label("Another"))
                                .on_click(move |_, _, cx| asset_store::make(&d, Make::Model, cx, |_, _| {})),
                        ))
                    });
            }
            _ => {
                let me = cx.entity().downgrade();
                let approved = a.model.clone().unwrap_or_default();
                let preview = a.models.iter().find(|m| m.file == approved).and_then(|m| m.preview.clone());
                let d = dir.to_path_buf();
                body = body.child(
                    div()
                        .flex()
                        .items_end()
                        .gap_3()
                        .children(preview.map(|p| self.asset_tile(id("final"), dir, &p, 150., true, cx)))
                        .child(div().flex_1())
                        .child(Button::new(id("view-approved")).small().outline().icon(gpui_kit::assets::IconName::Box).label("View in 3D").on_click(move |_, _, cx| {
                            let me = me.clone();
                            asset_store::view_3d(&d, &approved, cx, move |page, cx| {
                                let _ = me.update(cx, |_, cx| cx.emit(ChatPanelEvent::OpenWeb(keleuo_core::web::file_url(&page))));
                            });
                        })),
                );
            }
        }
        if let Some(err) = &e.error {
            body = body.child(div().text_xs().text_color(danger).child(err.clone()));
        }
        let kind = Some(capitalize(&a.kind));
        self.asset_frame(id("card"), a.name.clone(), kind, Some(a.stage), Some(dir.to_path_buf()), body.into_any_element(), cx)
    }
}

/// How far an asset has got: its steps in a row, the done ones ticked, the current one strong.
fn stepper(stage: Stage, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (fg, muted, green, border) = (theme.foreground, theme.muted_foreground, theme.green, theme.border);
    let mut row = div().flex().items_center().gap_1().text_xs();
    for (i, s) in [Stage::Concept, Stage::Views, Stage::Model, Stage::Rig].into_iter().enumerate() {
        if i > 0 {
            row = row.child(div().w(px(10.)).h(px(1.)).bg(if stage > s { green.opacity(0.6) } else { border }));
        }
        let done = stage > s;
        let now = stage == s;
        row = row.child(
            div()
                .flex()
                .items_center()
                .gap(px(3.))
                .px(px(6.))
                .py(px(2.))
                .rounded_full()
                .when(now, |d| d.bg(theme.secondary))
                .when(done, |d| d.child(Icon::new(IconName::Check).xsmall().text_color(green)))
                .child(div().text_color(if now { fg } else { muted }).when(now, |d| d.font_weight(FontWeight::MEDIUM)).child(s.label())),
        );
    }
    row.into_any_element()
}
