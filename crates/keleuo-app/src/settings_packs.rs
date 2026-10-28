//! Settings > Tools > Installed packs: tool packs ([`keleuo_core::packs`]) installed from a
//! folder. Each pack is a card: its name and whether it is set up, its services (images, audio)
//! each with its provider, keys (kept in the system's key store, never shown again) and
//! settings, then its tools and what it may do, folded away.
//!
//! Drawing reads nothing from disk or the key store: the packs and which keys are kept are
//! loaded once in the background (and again after an install or a removal).

use std::collections::{BTreeMap, HashMap, HashSet};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleuo_core::packs::{self, Pack, Service, Setting};

use crate::settings_window::{SettingsPage, change};

/// The section's own state: the installed packs (`None`: not loaded yet), its fields (one per
/// key and per setting), which parts are unfolded, what went wrong.
#[derive(Default)]
pub struct PacksState {
    packs: Option<Vec<Pack>>,
    loading: bool,
    fields: HashMap<String, Entity<InputState>>,
    open: HashSet<String>,
    /// Where each pack was installed from (by id), when that folder is still there: Update.
    sources: HashMap<String, std::path::PathBuf>,
    error: Option<String>,
    /// What the last update did (its version, any hosts it may now reach).
    notice: Option<String>,
}

impl PacksState {
    /// Unfold the part named `key` (for tests: `<pack id>/svc/<service>`, `<pack id>/tools`).
    pub fn unfold(&mut self, key: &str) {
        self.open.insert(key.to_string());
    }
}

fn field(state: &mut PacksState, key: &str, placeholder: &str, masked: bool, value: &str, window: &mut Window, cx: &mut App) -> Entity<InputState> {
    state
        .fields
        .entry(key.to_string())
        .or_insert_with(|| {
            let (placeholder, value) = (placeholder.to_string(), value.to_string());
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).masked(masked).default_value(value))
        })
        .clone()
}

fn set_error(page: &WeakEntity<SettingsPage>, error: Option<String>, cx: &mut App) {
    let _ = page.update(cx, |p, cx| {
        p.packs.error = error;
        p.packs.notice = None;
        cx.notify();
    });
}

/// Update `pack` in place from `from`, or from a folder asked for: keys and settings stay.
fn update(page: WeakEntity<SettingsPage>, pack: Pack, from: Option<std::path::PathBuf>, cx: &mut App) {
    let picked = from.is_none().then(|| cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Update pack".into()) }));
    cx.spawn(async move |cx: &mut AsyncApp| {
        let from = match (from, picked) {
            (Some(f), _) => f,
            (None, Some(picked)) => {
                let Ok(Ok(Some(paths))) = picked.await else { return };
                let Some(f) = paths.into_iter().next() else { return };
                f
            }
            (None, None) => return,
        };
        let Some(dir) = packs::dir() else { return };
        let done = packs::update_from_folder(&pack, &from, &dir);
        cx.update(|cx| {
            let _ = page.update(cx, |p, cx| {
                match done {
                    Ok(said) => {
                        p.packs.notice = Some(said);
                        p.packs.error = None;
                    }
                    Err(e) => {
                        p.packs.error = Some(e);
                        p.packs.notice = None;
                    }
                }
                cx.notify();
            });
            reload(&page, cx);
        });
    })
    .detach();
}

/// Fold or unfold the part named `key`.
fn toggle(page: &WeakEntity<SettingsPage>, key: &str, cx: &mut App) {
    let _ = page.update(cx, |p, cx| {
        if !p.packs.open.remove(key) {
            p.packs.open.insert(key.to_string());
        }
        cx.notify();
    });
}

/// Load the installed packs and look up their keys, off the main thread.
fn load(page: &WeakEntity<SettingsPage>, cx: &mut App) {
    let page = page.clone();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let found = cx
            .background_spawn(async move {
                let found = packs::dir().map(|d| packs::installed(&d)).unwrap_or_default();
                let mut sources = HashMap::new();
                for p in &found {
                    for s in &p.manifest.secrets {
                        keleuo_core::secrets::kept(&p.secret_account(&s.name));
                    }
                    if let Some(from) = p.source() {
                        sources.insert(p.manifest.id.clone(), from);
                    }
                }
                (found, sources)
            })
            .await;
        cx.update(|cx| {
            let (found, sources) = found;
            let _ = page.update(cx, |p, cx| {
                p.packs.packs = Some(found);
                p.packs.sources = sources;
                p.packs.loading = false;
                cx.notify();
            });
        })
    })
    .detach();
}

/// Load the packs again (after an install or a removal).
fn reload(page: &WeakEntity<SettingsPage>, cx: &mut App) {
    let _ = page.update(cx, |p, _| p.packs.packs = None);
    cx.refresh_windows();
}

/// Ask for a folder and install the pack in it.
fn install(page: WeakEntity<SettingsPage>, cx: &mut App) {
    let picked = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Install pack".into()) });
    cx.spawn(async move |cx: &mut AsyncApp| {
        let Ok(Ok(Some(paths))) = picked.await else { return };
        let Some(from) = paths.into_iter().next() else { return };
        let Some(dir) = packs::dir() else { return };
        let done = packs::install_from_folder(&from, &dir);
        cx.update(|cx| {
            set_error(&page, done.err(), cx);
            reload(&page, cx);
        });
    })
    .detach();
}

/// A rounded tile with an icon, beside a pack or a service.
fn tile(icon: Lucide, size: f32, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .text_color(theme.muted_foreground)
        .child(Icon::new(icon).size(px(size * 0.5)))
}

fn service_icon(service: &Service) -> Lucide {
    match service.icon.as_str() {
        "image" => Lucide::Image,
        "audio" => Lucide::AudioLines,
        "model" | "box" => Lucide::Box,
        _ => Lucide::Package,
    }
}

fn label_of(label: &str, name: &str) -> String {
    if label.is_empty() { name.to_string() } else { label.to_string() }
}

/// The section.
pub fn render(page: &WeakEntity<SettingsPage>, window: &mut Window, cx: &mut App) -> AnyElement {
    let Some(entity) = page.upgrade() else { return div().into_any_element() };
    let theme = cx.theme();
    let (fg, muted, border, danger) = (theme.foreground, theme.muted_foreground, theme.border, theme.danger);
    let state = &entity.read(cx).packs;
    let (installed, loading, error, notice) = (state.packs.clone(), state.loading, state.error.clone(), state.notice.clone());
    let p_install = page.clone();
    let mut head = div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).text_color(fg).child("Installed packs"))
        .child(div().px_2().rounded_full().bg(cx.theme().secondary).text_xs().text_color(muted).child(installed.as_ref().map(|p| p.len()).unwrap_or(0).to_string()))
        .child(div().flex_1());
    let has_packs = installed.as_ref().is_some_and(|p| !p.is_empty());
    if has_packs {
        head = head.child(Button::new("pack-install").small().outline().icon(IconName::Plus).label("Install pack").on_click(move |_, _, cx| install(p_install.clone(), cx)));
    }
    let mut section = div().w_full().flex().flex_col().gap_3().child(head);
    match installed {
        None => {
            if !loading {
                entity.update(cx, |p, _| p.packs.loading = true);
                load(page, cx);
            }
            section = section.child(div().text_sm().text_color(muted).child("Loading packs…"));
        }
        Some(installed) if installed.is_empty() => {
            let p_install = page.clone();
            section = section.child(
                div()
                    .py_6()
                    .rounded_lg()
                    .border_1()
                    .border_color(border)
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .child(tile(Lucide::Box, 44., cx))
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(fg).child("No tool packs installed"))
                    .child(div().text_sm().text_color(muted).child("Ready-made tools for every agent, run inside Keleuo: nothing else to install."))
                    .child(Button::new("pack-install").small().outline().icon(Icon::new(Lucide::FolderOpen)).label("Install from folder…").on_click(move |_, _, cx| install(p_install.clone(), cx))),
            );
        }
        Some(installed) => {
            let settings = cx.global::<crate::AppSettings>().0.borrow().clone();
            for pack in installed {
                section = section.child(card(page, &entity, &pack, &settings, window, cx));
            }
        }
    }
    let green = cx.theme().green;
    section
        .children(notice.map(|n| div().text_sm().text_color(green).child(n)))
        .children(error.map(|e| div().text_sm().text_color(danger).child(e)))
        .into_any_element()
}

fn card(page: &WeakEntity<SettingsPage>, entity: &Entity<SettingsPage>, pack: &Pack, settings: &keleuo_core::settings::Settings, window: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let (fg, muted, border, green, warning) = (theme.foreground, theme.muted_foreground, theme.border, theme.green, theme.warning);
    let m = &pack.manifest;
    let on = !settings.packs_off.contains(&m.id);
    // As loaded in the background (a key saved since is known at once).
    let has = |name: &str| keleuo_core::secrets::known(&pack.secret_account(name)).unwrap_or(false);
    // Its settings as they stand: what to show depends on them (no Azure fields for OpenAI).
    let values = pack.values(settings.pack_settings.get(&m.id));
    let open = entity.read(cx).packs.open.clone();

    // Set up: every service with nothing missing (a pack without services: its keys).
    let services: Vec<Service> = if m.services.is_empty() { vec![Service { name: "Keys".into(), ..Default::default() }] } else { m.services.clone() };
    let ready: Vec<bool> = services.iter().map(|s| pack.missing(&s.id, &values, &has).is_empty()).collect();
    let set_up = ready.iter().filter(|r| **r).count();
    let (dot, status, hint) = match set_up {
        0 => (warning, "Setup needed".to_string(), "Set up a service to make its tools available."),
        n if n == services.len() => (green, "Ready".to_string(), "Every service is set up."),
        n => (green, format!("{n} of {} services set up", services.len()), "The others' tools stay hidden until they are."),
    };

    let (id_on, id_remove, dir, p_remove) = (m.id.clone(), m.id.clone(), pack.dir.clone(), page.clone());
    let source = entity.read(cx).packs.sources.get(&m.id).cloned();
    let (p_update, pack_update) = (page.clone(), pack.clone());
    let header = div()
        .flex()
        .items_start()
        .gap_3()
        .child(tile(Lucide::Package, 44., cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(m.name.clone()))
                .child(div().text_xs().text_color(muted).child(format!("v{} · {}", m.version, m.id)))
                .when(!m.description.is_empty(), |d| d.child(div().pt_1().text_sm().text_color(fg).child(m.description.clone())))
                .child(
                    div()
                        .pt_1()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_sm()
                        .child(div().size(px(8.)).rounded_full().bg(dot))
                        .child(div().text_color(if set_up == 0 { warning } else { green }).child(status))
                        .child(div().text_color(border).child("|"))
                        .child(div().text_color(muted).child(hint)),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().text_sm().text_color(muted).child(if on { "Enabled" } else { "Off" }))
                .child(Switch::new(SharedString::from(format!("pack-on-{}", m.id))).checked(on).on_click(move |on: &bool, _, cx| {
                    let (id, on) = (id_on.clone(), *on);
                    change(cx, move |s| {
                        s.packs_off.retain(|x| *x != id);
                        if !on {
                            s.packs_off.push(id);
                        }
                    });
                }))
                .child(Button::new(SharedString::from(format!("pack-more-{}", m.id))).ghost().small().icon(IconName::Ellipsis).dropdown_menu(move |mut menu, _, _| {
                    // Update in place (keys and settings stay): from where it came, or a folder.
                    if let Some(from) = source.clone() {
                        let name = from.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                        let (page, pack) = (p_update.clone(), pack_update.clone());
                        menu = menu.item(PopupMenuItem::new(format!("Update from {name}")).on_click(move |_, _, cx| update(page.clone(), pack.clone(), Some(from.clone()), cx)));
                    }
                    let (page, pack) = (p_update.clone(), pack_update.clone());
                    menu = menu.item(PopupMenuItem::new("Update from folder…").on_click(move |_, _, cx| update(page.clone(), pack.clone(), None, cx)));
                    let (dir, id, page) = (dir.clone(), id_remove.clone(), p_remove.clone());
                    menu.separator().item(PopupMenuItem::new("Remove pack").on_click(move |_, _, cx| {
                        let _ = std::fs::remove_dir_all(&dir);
                        reload(&page, cx);
                        let id = id.clone();
                        change(cx, move |s| s.packs_off.retain(|x| *x != id));
                    }))
                })),
        );

    // The services, one row each, unfolding into their keys and settings.
    let mut rows = div().flex().flex_col().rounded_lg().border_1().border_color(border);
    for (i, service) in services.iter().enumerate() {
        let key = format!("{}/svc/{}", m.id, service.id);
        let unfolded = open.contains(&key);
        rows = rows.child(service_row(page, entity, pack, service, ready[i], unfolded, &key, &values, i > 0, window, cx));
    }

    // The pack's own settings (where to save), beside no service.
    let mut general = div().flex().flex_col().gap_1();
    // (Without services, the one row above has them.)
    for st in m.settings.iter().filter(|s| !m.services.is_empty() && s.service.is_empty() && packs::shown(&s.when, &values)) {
        general = general.child(setting_row(entity, pack, st, &values, settings, window, cx));
    }

    // Its tools, folded: ✓ ready (its service set up), ○ not yet.
    let tools_key = format!("{}/tools", m.id);
    let ready_tools = m.tools.iter().filter(|t| Pack::ready(t, &has)).count();
    let mut tools = div().flex().flex_col().gap_1().pl_6();
    if open.contains(&tools_key) {
        for t in &m.tools {
            let ok = Pack::ready(t, &has);
            tools = tools.child(
                div()
                    .flex()
                    .gap_2()
                    .text_sm()
                    .child(div().flex_none().w(px(14.)).text_color(if ok { green } else { muted }).child(if ok { "✓" } else { "○" }))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().font_weight(FontWeight::MEDIUM).text_color(if ok { fg } else { muted }).child(t.name.clone()))
                            .child(div().text_xs().text_color(muted).child(if t.cost.is_empty() { t.description.clone() } else { format!("{} Cost: {}.", t.description, t.cost) })),
                    ),
            );
        }
    }

    // What it may do, folded.
    let about_key = format!("{}/about", m.id);
    let mut about = div().flex().flex_col().gap_1().pl_6().text_sm().text_color(muted);
    if open.contains(&about_key) {
        about = about
            .child("Runs inside Keleuo, with no access to your files beyond the session's folder, where it saves what it makes.")
            .child("Reads only its own keys, and only for its own requests: what it returns never contains them.")
            .child("Tools that cost money ask before each run; \"Always for this session\" stops asking.");
    }

    div()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(border)
        .when(!on, |d| d.opacity(0.6))
        .flex()
        .flex_col()
        .gap_3()
        .child(header)
        .child(div().h(px(1.)).bg(border))
        .when(!m.services.is_empty(), |d| {
            d.child(
                div()
                    .flex()
                    .flex_col()
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(fg).child("Services"))
                    .child(div().text_sm().text_color(muted).child("Set up only the services you want to use.")),
            )
        })
        .child(rows)
        .child(general)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .text_color(muted)
                .child(Icon::new(Lucide::Lock).small())
                .child("Keys stay in your system's key store. Agents never see them."),
        )
        .when(!m.network.is_empty(), |d| {
            // And the addresses you give it (an endpoint), exactly those hosts.
            let yours: Vec<String> = m.settings.iter().filter(|s| s.address).map(|s| label_of(&s.label, &s.name)).collect();
            let mut text = format!("May reach: {}", m.network.join(", "));
            if !yours.is_empty() {
                text.push_str(&format!(", and the address you give as {}", yours.join(", ")));
            }
            d.child(div().text_xs().text_color(muted).child(text))
        })
        .child(div().h(px(1.)).bg(border))
        .child(fold(page, &tools_key, "Included tools", &format!("{} tools · {ready_tools} ready", m.tools.len()), open.contains(&tools_key), cx))
        .when(open.contains(&tools_key), |d| d.child(tools))
        .child(fold(page, &about_key, "Permissions & usage", "", open.contains(&about_key), cx))
        .when(open.contains(&about_key), |d| d.child(about))
        .into_any_element()
}

/// A row that folds and unfolds the part named `key`.
fn fold(page: &WeakEntity<SettingsPage>, key: &str, title: &str, note: &str, unfolded: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (p, k) = (page.clone(), key.to_string());
    div()
        .id(SharedString::from(format!("pack-fold-{key}")))
        .flex()
        .items_center()
        .gap_2()
        .py_1()
        .cursor_pointer()
        .child(Icon::new(if unfolded { IconName::ChevronDown } else { IconName::ChevronRight }).small().text_color(theme.muted_foreground))
        .child(div().flex_1().text_color(theme.foreground).child(title.to_string()))
        .child(div().text_sm().text_color(theme.muted_foreground).child(note.to_string()))
        .on_click(move |_, _, cx| toggle(&p, &k, cx))
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn service_row(
    page: &WeakEntity<SettingsPage>,
    entity: &Entity<SettingsPage>,
    pack: &Pack,
    service: &Service,
    ready: bool,
    unfolded: bool,
    key: &str,
    values: &BTreeMap<String, String>,
    divided: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.theme();
    let (fg, muted, border, green, secondary) = (theme.foreground, theme.muted_foreground, theme.border, theme.green, theme.secondary);
    let m = &pack.manifest;
    let settings = cx.global::<crate::AppSettings>().0.borrow().clone();
    let (p_fold, k_fold) = (page.clone(), key.to_string());
    let mut right = div().flex().items_center().gap_2();
    // The provider, on the row itself (OpenAI or Azure).
    if let Some(st) = m.settings.iter().find(|s| !service.choose.is_empty() && s.name == service.choose) {
        right = right.child(choice(pack, st, values));
    }
    right = right.child(if ready {
        div().flex().items_center().gap_1().text_sm().text_color(green).child("✓").child("Set up").into_any_element()
    } else {
        div().text_sm().text_color(muted).child("Not set up").into_any_element()
    });
    if !ready && !unfolded {
        let (p, k) = (page.clone(), key.to_string());
        right = right.child(Button::new(SharedString::from(format!("pack-add-{key}"))).small().outline().label("Add key").on_click(move |_, _, cx| toggle(&p, &k, cx)));
    }
    right = right.child(
        Button::new(SharedString::from(format!("pack-chev-{key}")))
            .ghost()
            .small()
            .icon(if unfolded { IconName::ChevronUp } else { IconName::ChevronDown })
            .on_click(move |_, _, cx| toggle(&p_fold, &k_fold, cx)),
    );
    let row = div()
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .py_3()
        .child(tile(service_icon(service), 36., cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(service.name.clone()))
                .when(!service.description.is_empty(), |d| d.child(div().text_sm().text_color(muted).child(service.description.clone()))),
        )
        .child(right);

    let mut body = div().flex().flex_col().gap_2();
    if unfolded {
        // Its keys: a field each, saved to the key store (a kept one shows as kept, never its value).
        for s in m.secrets.iter().filter(|s| s.service == service.id && packs::shown(&s.when, values)) {
            let account = pack.secret_account(&s.name);
            let kept = keleuo_core::secrets::known(&account).unwrap_or(false);
            let input = entity.update(cx, |p, cx| field(&mut p.packs, &format!("{}/secret/{}", m.id, s.name), if kept { "Saved; paste a new key to replace it" } else { "Paste the key" }, true, "", window, cx));
            let (inp, acct, pg) = (input.clone(), account.clone(), page.clone());
            let help = s.help.clone();
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .child(div().font_weight(FontWeight::MEDIUM).text_color(fg).child(label_of(&s.label, &s.name)))
                            .when(kept, |d| d.child(div().text_color(green).child("✓ Saved"))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().child(Input::new(&input)))
                            .child(Button::new(SharedString::from(format!("pack-key-{account}"))).primary().label("Save key").on_click(move |_, window, cx| {
                                let value = inp.read(cx).value().trim().to_string();
                                if value.is_empty() {
                                    return;
                                }
                                let saved = keleuo_core::secrets::set(&acct, &value).map_err(|e| format!("Could not keep the key: {e}"));
                                inp.update(cx, |i, cx| i.set_value("", window, cx));
                                set_error(&pg, saved.err(), cx);
                                cx.refresh_windows();
                            })),
                    )
                    .when(!help.is_empty(), |d| {
                        let url = help.clone();
                        d.child(
                            div()
                                .id(SharedString::from(format!("pack-help-{account}")))
                                .text_xs()
                                .text_color(muted)
                                .cursor_pointer()
                                .hover(|s| s.underline())
                                .child(format!("Get a key: {help}"))
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                    }),
            );
        }
        // Its settings; the advanced ones folded.
        let shown: Vec<&Setting> = m.settings.iter().filter(|s| s.service == service.id && s.name != service.choose && packs::shown(&s.when, values)).collect();
        for st in shown.iter().filter(|s| !s.advanced) {
            body = body.child(setting_row(entity, pack, st, values, &settings, window, cx));
        }
        let advanced: Vec<&&Setting> = shown.iter().filter(|s| s.advanced).collect();
        if !advanced.is_empty() {
            let adv_key = format!("{key}/advanced");
            let adv_open = entity.read(cx).packs.open.contains(&adv_key);
            body = body.child(fold(page, &adv_key, "Advanced", "", adv_open, cx));
            if adv_open {
                for st in advanced {
                    body = body.child(setting_row(entity, pack, st, values, &settings, window, cx));
                }
            }
        }
        let names: Vec<String> = m.tools.iter().filter(|t| t.service == service.id).map(|t| t.name.clone()).collect();
        if !names.is_empty() {
            body = body.child(div().text_xs().text_color(muted).child(format!("Tools: {}", names.join(", "))));
        }
    }

    div()
        .flex()
        .flex_col()
        .when(divided, |d| d.border_t_1().border_color(border))
        .child(row)
        .when(unfolded, |d| d.child(div().mx_3().mb_3().p_3().rounded_md().bg(secondary).child(body)))
        .into_any_element()
}

/// A setting with choices: a dropdown, saved when picked.
fn choice(pack: &Pack, st: &Setting, values: &BTreeMap<String, String>) -> AnyElement {
    let current = values.get(&st.name).cloned().unwrap_or_default();
    let show = |c: &str| st.labels.get(c).cloned().unwrap_or_else(|| c.to_string());
    let items: Vec<(String, String)> = st.choices.iter().map(|c| (c.clone(), show(c))).collect();
    let (pid, name) = (pack.manifest.id.clone(), st.name.clone());
    Button::new(SharedString::from(format!("pack-choice-{pid}-{name}")))
        .label(if current.is_empty() { "Choose".to_string() } else { show(&current) })
        .small()
        .outline()
        .dropdown_caret(true)
        .dropdown_menu(move |mut menu, _, _| {
            for (c, shown) in &items {
                let (pid, name, pick) = (pid.clone(), name.clone(), c.clone());
                menu = menu.item(PopupMenuItem::new(shown.clone()).checked(*c == current).on_click(move |_, _, cx| {
                    let (pid, name, pick) = (pid.clone(), name.clone(), pick.clone());
                    change(cx, move |s| {
                        s.pack_settings.entry(pid).or_default().insert(name, pick);
                    });
                }));
            }
            menu
        })
        .into_any_element()
}

/// A setting's row: its label, then a dropdown or a field with Save.
fn setting_row(entity: &Entity<SettingsPage>, pack: &Pack, st: &Setting, values: &BTreeMap<String, String>, settings: &keleuo_core::settings::Settings, window: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let fg = theme.foreground;
    let m = &pack.manifest;
    let label = div().w(px(150.)).flex_none().text_sm().text_color(fg).child(label_of(&st.label, &st.name));
    if !st.choices.is_empty() && !st.custom {
        return div().flex().items_center().gap_2().child(label).child(choice(pack, st, values)).into_any_element();
    }
    let current = settings.pack_settings.get(&m.id).and_then(|s| s.get(&st.name)).cloned().unwrap_or_default();
    let placeholder = if st.placeholder.is_empty() { st.default.clone() } else { st.placeholder.clone() };
    let input = entity.update(cx, |p, cx| field(&mut p.packs, &format!("{}/setting/{}", m.id, st.name), &placeholder, false, &current, window, cx));
    let (inp, pid, name) = (input.clone(), m.id.clone(), st.name.clone());
    // Choices that are only suggestions: a dropdown beside the field puts one in it and saves.
    let suggest = (!st.choices.is_empty()).then(|| {
        let (choices, inp, pid, name) = (st.choices.clone(), input.clone(), m.id.clone(), st.name.clone());
        let current = values.get(&st.name).cloned().unwrap_or_default();
        Button::new(SharedString::from(format!("pack-suggest-{pid}-{name}"))).small().ghost().icon(IconName::ChevronDown).tooltip("Known values").dropdown_menu(move |mut menu, _, _| {
            for c in &choices {
                let (inp, pid, name, pick) = (inp.clone(), pid.clone(), name.clone(), c.clone());
                menu = menu.item(PopupMenuItem::new(c.clone()).checked(*c == current).on_click(move |_, window, cx| {
                    inp.update(cx, |i, cx| i.set_value(pick.clone(), window, cx));
                    let (pid, name, pick) = (pid.clone(), name.clone(), pick.clone());
                    change(cx, move |s| {
                        s.pack_settings.entry(pid).or_default().insert(name, pick);
                    });
                }));
            }
            menu
        })
    });
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(label)
        .child(div().flex_1().child(Input::new(&input).small()))
        .children(suggest)
        .child(Button::new(SharedString::from(format!("pack-set-{pid}-{name}"))).small().outline().label("Save").on_click(move |_, _, cx| {
            let value = inp.read(cx).value().trim().to_string();
            let (pid, name) = (pid.clone(), name.clone());
            change(cx, move |s| {
                s.pack_settings.entry(pid).or_default().insert(name, value);
            });
        }))
        .into_any_element()
}
