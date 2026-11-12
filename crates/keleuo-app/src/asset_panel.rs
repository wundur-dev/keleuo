//! A game asset being made, as a tab (see `keleuo_core::assets` and
//! `docs/proposals/asset-pipeline.md`): its steps across the top, the current one below.
//! Concept: low-resolution tries from the brief, one picked and changed with a note until it is
//! right, then approved. Views: front, side and back from the approved concept, each approved
//! on its own. What each try cost shows, and the total. A new asset starts with a short form.

use std::path::{Path, PathBuf};

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent, PanelInfo, PanelState};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleuo_core::assets::{self, Asset, Make, Stage, View};

/// What the workspace hears from an asset tab.
pub enum AssetEvent {
    Activated,
    /// Its folder changed (a new asset was created): the layout keeps it.
    Changed,
    Closed,
}

impl EventEmitter<AssetEvent> for AssetPanel {}
impl EventEmitter<PanelEvent> for AssetPanel {}

pub struct AssetPanel {
    /// The session's folder; the asset's folder inside it (empty while the form is up).
    project: PathBuf,
    pub dir: PathBuf,
    asset: Asset,
    /// The step shown (the asset's own, unless the person looked at another).
    shown: Stage,
    /// The concept picked to change or approve.
    picked: Option<String>,
    /// What is being made now, and what went wrong last.
    busy: Option<String>,
    error: Option<String>,
    name: Entity<InputState>,
    brief: Entity<InputState>,
    note: Entity<InputState>,
    kind: String,
    focus: FocusHandle,
}

const KINDS: [&str; 4] = ["character", "creature", "prop", "environment"];

impl AssetPanel {
    /// The asset in `dir` (a folder with its record), or the form for a new one in `project`
    /// when `dir` is `None`.
    pub fn new(project: PathBuf, dir: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let asset = dir.as_deref().and_then(assets::load).unwrap_or_default();
        let field = |placeholder: &str, value: &str, window: &mut Window, cx: &mut Context<Self>| {
            let (p, v) = (placeholder.to_string(), value.to_string());
            cx.new(|cx| InputState::new(window, cx).placeholder(p).default_value(v))
        };
        let name = field("Name, e.g. Knight", &asset.name, window, cx);
        let brief = field("What it is, in detail: look, colours, style, clothes or materials", &asset.brief, window, cx);
        let note = field("What to change, e.g. a blue cape, shorter sword", "", window, cx);
        let picked = asset.concept.clone().or_else(|| asset.concepts.last().map(|a| a.file.clone()));
        Self {
            project,
            shown: asset.stage.min(Stage::Views),
            kind: if asset.kind.is_empty() { "character".into() } else { asset.kind.clone() },
            dir: dir.unwrap_or_default(),
            asset,
            picked,
            busy: None,
            error: None,
            name,
            brief,
            note,
            focus: cx.focus_handle(),
        }
    }

    fn is_new(&self) -> bool {
        self.dir.as_os_str().is_empty()
    }

    fn title_text(&self) -> String {
        if self.is_new() { "New asset".into() } else { self.asset.name.clone() }
    }

    fn save(&mut self) {
        if let Err(e) = assets::save(&self.dir, &self.asset) {
            self.error = Some(format!("Couldn't save the asset: {e}"));
        }
    }

    /// The form's Create: the asset's folder and record.
    fn create(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_string();
        let brief = self.brief.read(cx).value().trim().to_string();
        if name.is_empty() || brief.is_empty() {
            self.error = Some("Give it a name and say what it is.".into());
            cx.notify();
            return;
        }
        let mut dir = assets::folder_for(&self.project, &self.kind, &name);
        // Another asset of that name: a folder of its own.
        let mut n = 2;
        while dir.join(assets::RECORD).exists() {
            dir = assets::folder_for(&self.project, &self.kind, &format!("{name} {n}"));
            n += 1;
        }
        self.dir = dir;
        self.asset = Asset { name, kind: self.kind.clone(), brief, ..Default::default() };
        self.error = None;
        self.save();
        cx.emit(AssetEvent::Changed);
        cx.notify();
    }

    /// Make `what` off the UI thread, through the game-assets pack with the person's keys.
    fn make(&mut self, what: Make, label: String, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let Some(pack) = packs_dir_pack() else {
            self.error = Some("Install the Game assets pack (Settings > Tools) and add your OpenAI or Azure key.".into());
            cx.notify();
            return;
        };
        let settings = cx.global::<crate::AppSettings>().0.borrow().pack_settings.get(assets::PACK).cloned().unwrap_or_default();
        let (project, dir, mut asset) = (self.project.clone(), self.dir.clone(), self.asset.clone());
        self.busy = Some(label);
        self.error = None;
        cx.notify();
        let (tx, rx) = async_channel::bounded(1);
        // Test copies (KELEUO_TEST_BACKENDS) may use a stand-in service on this machine, with a
        // stand-in key (KELEUO_TEST_PACK_KEY), never the person's.
        let test = keleuo_core::env_var("TEST_BACKENDS").is_ok();
        let test_key = test.then(|| keleuo_core::env_var("TEST_PACK_KEY").ok()).flatten();
        std::thread::spawn(move || {
            let secret = |name: &str| match &test_key {
                Some(k) => Some(k.clone()),
                None => keleuo_core::secrets::get(&pack.secret_account(name)),
            };
            let (extra_hosts, allow_http) = if test { (vec!["127.0.0.1".to_string()], true) } else { (Vec::new(), false) };
            let grant = keleuo_core::packs::Grant { cwd: &project, secret: &secret, settings: &settings, extra_hosts, allow_http };
            let made = assets::make(&dir, &mut asset, &what, &pack, &grant);
            let _ = tx.send_blocking((made, asset));
        });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let Ok((made, asset)) = rx.recv().await else { return };
            let _ = this.update(cx, |p, cx| {
                p.busy = None;
                match made {
                    Ok(attempt) => {
                        p.asset = asset;
                        if attempt.view.is_none() {
                            p.picked = Some(attempt.file);
                        }
                        p.save();
                    }
                    Err(e) => p.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Test commands: `new <name>|<brief>`, `concept`, `change <note>`, `approve`, `views`,
    /// `approve-view <front|side|back>`, `state`.
    pub fn test_command(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
        match cmd {
            "new" => {
                let (name, brief) = arg.split_once('|').unwrap_or((arg, arg));
                self.name.update(cx, |i, cx| i.set_value(name.to_string(), window, cx));
                self.brief.update(cx, |i, cx| i.set_value(brief.to_string(), window, cx));
                self.create(cx);
            }
            "concept" => self.make(Make::Concept { prompt: self.brief.read(cx).value().to_string() }, "a concept".into(), cx),
            "change" => {
                if let Some(from) = self.picked.clone() {
                    self.make(Make::Change { from, note: arg.to_string() }, "the change".into(), cx);
                }
            }
            "approve" => {
                if let Some(f) = self.picked.clone() {
                    self.asset.approve_concept(&f);
                    self.shown = Stage::Views;
                    self.save();
                }
            }
            "views" => self.make_views_from(0, cx),
            "view" => {
                if let Some(v) = View::ALL.iter().find(|v| v.label().eq_ignore_ascii_case(arg)) {
                    self.make(Make::View(*v), format!("the {arg} view"), cx);
                }
            }
            "approve-view" => {
                if let Some(v) = View::ALL.iter().find(|v| v.label().eq_ignore_ascii_case(arg))
                    && let Some(f) = self.asset.views.iter().rev().find(|a| a.view == Some(*v)).map(|a| a.file.clone())
                {
                    self.asset.approve_view(*v, &f);
                    self.save();
                }
            }
            "state" => {}
            _ => return format!("unknown asset command: {cmd}"),
        }
        cx.notify();
        format!(
            "stage {:?}, {} concepts, {} views, picked {:?}, busy {:?}, error {:?}, {:.0}¢",
            self.asset.stage,
            self.asset.concepts.len(),
            self.asset.views.len(),
            self.picked,
            self.busy,
            self.error,
            self.asset.cents()
        )
    }

    // ----- drawing -----

    fn render_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted) = (theme.foreground, theme.muted_foreground);
        let kind = self.kind.clone();
        let mut kinds = div().flex().gap_1();
        for k in KINDS {
            let on = kind == k;
            kinds = kinds.child(
                Button::new(SharedString::from(format!("asset-kind-{k}")))
                    .small()
                    .label(capitalize(k))
                    .when(on, |b| b.primary())
                    .when(!on, |b| b.outline())
                    .on_click(cx.listener(move |p, _, _, cx| {
                        p.kind = k.to_string();
                        cx.notify();
                    })),
            );
        }
        div()
            .p_4()
            .max_w(px(640.))
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).text_color(fg).child("New asset"))
            .child(div().text_sm().text_color(muted).child(
                "Made step by step, you approving each: a concept image (cheap tries until it's right), then front, side and back views, then the 3D model and its rig.",
            ))
            .child(label("Name", cx))
            .child(Input::new(&self.name))
            .child(label("What it is", cx))
            .child(Input::new(&self.brief))
            .child(label("Kind", cx))
            .child(kinds)
            .child(div().flex().child(Button::new("asset-create").primary().label("Create").on_click(cx.listener(|p, _, _, cx| p.create(cx)))))
            .into_any_element()
    }

    fn render_steps(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let mut row = div().flex().items_center().gap_1();
        for (i, stage) in [Stage::Concept, Stage::Views, Stage::Model, Stage::Rig].into_iter().enumerate() {
            if i > 0 {
                row = row.child(Icon::new(IconName::ChevronRight).xsmall().text_color(muted));
            }
            let done = self.asset.stage > stage;
            let reachable = self.asset.stage >= stage;
            row = row.child(
                Button::new(SharedString::from(format!("asset-step-{}", stage.label())))
                    .small()
                    .when(self.shown == stage, |b| b.primary())
                    .when(self.shown != stage, |b| b.ghost())
                    .when(done, |b| b.icon(IconName::Check))
                    .label(stage.label())
                    .disabled(!reachable)
                    .on_click(cx.listener(move |p, _, _, cx| {
                        p.shown = stage;
                        cx.notify();
                    })),
            );
        }
        row.into_any_element()
    }

    /// A thumbnail of a file in the asset's folder: picked when `picked`; a click calls `on`.
    fn thumb(&self, id: String, file: &str, size: f32, picked: bool, cents: Option<f32>, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme();
        let (ring, border, muted) = (theme.ring, theme.border, theme.muted_foreground);
        let path = self.dir.join(file);
        let view = path.clone();
        div()
            .id(SharedString::from(id))
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .size(px(size))
                    .rounded_md()
                    .overflow_hidden()
                    .border_2()
                    .border_color(if picked { ring } else { border })
                    .bg(theme.secondary)
                    .cursor_pointer()
                    .child(img(path).size_full().object_fit(ObjectFit::Contain))
                    .on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, _, cx| {
                        if e.click_count >= 2 {
                            crate::image_view::show(view.clone(), cx);
                        }
                    }),
            )
            .children(cents.map(|c| div().text_xs().text_color(muted).child(format!("≈ {}", money(c)))))
    }

    fn render_concept(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, green) = (theme.foreground, theme.muted_foreground, theme.green);
        let busy = self.busy.is_some();
        let mut tries = div().flex().flex_wrap().gap_3();
        for (i, a) in self.asset.concepts.iter().enumerate() {
            let file = a.file.clone();
            let picked = self.picked.as_deref() == Some(a.file.as_str());
            let approved = self.asset.concept.as_deref() == Some(a.file.as_str());
            tries = tries.child(
                self.thumb(format!("concept-{i}"), &a.file, 150., picked, Some(a.cents), cx)
                    .when(approved, |d| d.child(div().text_xs().text_color(green).child("✓ Approved")))
                    .on_click(cx.listener(move |p, _, _, cx| {
                        p.picked = Some(file.clone());
                        cx.notify();
                    })),
            );
        }
        let has = !self.asset.concepts.is_empty();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(label("What it is", cx))
            .child(Input::new(&self.brief))
            .child(
                div().flex().items_center().gap_2().child(
                    Button::new("asset-concept")
                        .primary()
                        .label(if has { "Make another" } else { "Make a concept" })
                        .disabled(busy)
                        .on_click(cx.listener(|p, _, _, cx| {
                            let prompt = p.brief.read(cx).value().to_string();
                            p.asset.brief = prompt.clone();
                            p.make(Make::Concept { prompt }, "a concept".into(), cx);
                        })),
                )
                .child(div().text_xs().text_color(muted).child("low resolution, about 1¢ each")),
            )
            .when(!has, |d| d.child(div().text_sm().text_color(muted).child("Try a few: they're cheap. Pick the closest, then change it with a note.")))
            .when(has, |d| {
                d.child(tries)
                    .child(div().text_xs().text_color(muted).child("Click to pick one; double-click to see it full size."))
                    .child(label("Change the picked one", cx))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().child(Input::new(&self.note)))
                            .child(Button::new("asset-change").outline().label("Change it").disabled(busy || self.picked.is_none()).on_click(cx.listener(|p, _, window, cx| {
                                let note = p.note.read(cx).value().trim().to_string();
                                if note.is_empty() {
                                    return;
                                }
                                if let Some(from) = p.picked.clone() {
                                    p.note.update(cx, |i, cx| i.set_value("", window, cx));
                                    p.make(Make::Change { from, note }, "the change".into(), cx);
                                }
                            }))),
                    )
                    .child(div().flex().child(Button::new("asset-approve").primary().icon(IconName::Check).label("Approve the picked one").disabled(busy || self.picked.is_none()).on_click(
                        cx.listener(|p, _, _, cx| {
                            if let Some(f) = p.picked.clone() {
                                p.asset.approve_concept(&f);
                                p.shown = Stage::Views;
                                p.save();
                                cx.notify();
                            }
                        }),
                    )))
            })
            .text_color(fg)
            .into_any_element()
    }

    fn render_views(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, green, border) = (theme.muted_foreground, theme.green, theme.border);
        let busy = self.busy.is_some();
        let mut slots = div().flex().flex_wrap().gap_4();
        for v in View::ALL {
            let tries: Vec<&assets::Attempt> = self.asset.views.iter().filter(|a| a.view == Some(v)).collect();
            let approved = self.asset.approved_view(v).map(str::to_string);
            let shown = approved.clone().or_else(|| tries.last().map(|a| a.file.clone()));
            let mut slot = div().flex().flex_col().gap_2().child(div().font_weight(FontWeight::MEDIUM).child(v.label()));
            slot = match &shown {
                Some(file) => slot.child(self.thumb(format!("view-{}", v.label()), file, 170., approved.is_some(), None, cx)),
                None => slot.child(div().size(px(170.)).rounded_md().border_1().border_color(border).flex().items_center().justify_center().text_sm().text_color(muted).child("Not made yet")),
            };
            if tries.len() > 1 {
                slot = slot.child(div().text_xs().text_color(muted).child(format!("{} tries", tries.len())));
            }
            let file = shown.clone();
            slot = slot
                .child(
                    Button::new(SharedString::from(format!("view-make-{}", v.label())))
                        .small()
                        .outline()
                        .label(if tries.is_empty() { "Make" } else { "Try again" })
                        .disabled(busy)
                        .on_click(cx.listener(move |p, _, _, cx| p.make(Make::View(v), format!("the {} view", v.label().to_lowercase()), cx))),
                )
                .when(approved.is_some(), |d| d.child(div().text_xs().text_color(green).child("✓ Approved")))
                .when(approved.is_none() && file.is_some(), |d| {
                    d.child(Button::new(SharedString::from(format!("view-approve-{}", v.label()))).small().primary().label("Approve").on_click(cx.listener(move |p, _, _, cx| {
                        if let Some(f) = &file {
                            p.asset.approve_view(v, f);
                            p.save();
                            cx.notify();
                        }
                    })))
                });
            slots = slots.child(slot);
        }
        let concept = self.asset.concept.clone().unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(self.thumb("view-concept".into(), &concept, 70., false, None, cx))
                    .child(div().flex_1().min_w_0().flex().flex_col().gap_1().child(div().text_sm().child("From the approved concept")).child(div().text_xs().text_color(muted).child(
                        "Three views for the 3D model, the same design from the front, side and back. Medium resolution, about 10¢ each.",
                    ))),
            )
            .child(div().flex().child(Button::new("views-all").primary().label("Make all three").disabled(busy).on_click(cx.listener(|p, _, _, cx| {
                // One after another: each its own request.
                p.make_views_from(0, cx);
            }))))
            .child(slots)
            .into_any_element()
    }

    /// The views not made yet, one after another, from `from` in `View::ALL`.
    fn make_views_from(&mut self, from: usize, cx: &mut Context<Self>) {
        let Some(v) = View::ALL.get(from).copied() else { return };
        if self.asset.views.iter().any(|a| a.view == Some(v)) {
            return self.make_views_from(from + 1, cx);
        }
        self.make(Make::View(v), format!("the {} view", v.label().to_lowercase()), cx);
        // The next once this one is done.
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
                let Ok(done) = this.update(cx, |p, _| p.busy.is_none()) else { return };
                if done {
                    let _ = this.update(cx, |p, cx| {
                        if p.error.is_none() {
                            p.make_views_from(from + 1, cx);
                        }
                    });
                    return;
                }
            }
        })
        .detach();
    }

    fn render_later(&self, what: &str, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        div().text_sm().text_color(muted).child(format!("{what}: coming next (Tripo from the three views, then the rig).")).into_any_element()
    }
}

/// The game-assets pack, when installed.
fn packs_dir_pack() -> Option<keleuo_core::packs::Pack> {
    let dir = keleuo_core::packs::dir()?;
    keleuo_core::packs::installed(&dir).into_iter().find(|p| p.manifest.id == assets::PACK)
}

fn label(text: &str, cx: &App) -> Div {
    div().text_sm().font_weight(FontWeight::MEDIUM).text_color(cx.theme().foreground).child(text.to_string())
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// Cents as money: "3¢", "$1.20".
fn money(cents: f32) -> String {
    // (+ 0.0: an empty sum of floats is -0.0, which would show as "-0¢".)
    let cents = cents.max(0.0) + 0.0;
    if cents < 100.0 { format!("{cents:.0}¢") } else { format!("${:.2}", cents / 100.0) }
}

impl Focusable for AssetPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AssetPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, fg, muted, danger) = (theme.background, theme.foreground, theme.muted_foreground, theme.danger);
        let root = div().id("asset-panel").track_focus(&self.focus).size_full().bg(bg).text_color(fg).overflow_y_scroll();
        if self.is_new() {
            return root.child(self.render_form(cx)).children(self.error.clone().map(|e| div().px_4().text_sm().text_color(danger).child(e)));
        }
        let body = match self.shown {
            Stage::Concept => self.render_concept(cx),
            Stage::Views => self.render_views(cx),
            Stage::Model => self.render_later("Model", cx),
            Stage::Rig | Stage::Done => self.render_later("Rig", cx),
        };
        root.child(
            div()
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(self.asset.name.clone()))
                        .child(div().text_sm().text_color(muted).child(capitalize(&self.asset.kind)))
                        .child(div().flex_1())
                        .child(div().text_sm().text_color(muted).child(format!("≈ {} so far", money(self.asset.cents())))),
                )
                .child(self.render_steps(cx))
                .children(self.busy.clone().map(|what| div().flex().items_center().gap_2().text_sm().text_color(muted).child(Icon::new(IconName::Loader).small()).child(format!("Making {what}…"))))
                .children(self.error.clone().map(|e| div().text_sm().text_color(danger).child(e)))
                .child(body),
        )
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
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().flex().items_center().gap_1().child(Icon::new(gpui_kit::assets::IconName::Box).xsmall()).child(div().max_w(px(180.)).truncate().child(self.title_text()))
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

/// Whether `path` is an asset's record (opening it opens the asset).
pub fn is_record(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == assets::RECORD)
}
