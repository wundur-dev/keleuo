//! Settings > Tools > Packs: tool packs ([`keleuo_core::packs`]) installed from a folder, each
//! with its tools (and what they cost), its keys (kept in the system's key store, never shown
//! again), its settings, a switch, and Remove.

use std::collections::HashMap;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleuo_core::packs::{self, Pack};

use crate::settings_window::{SettingsPage, change};

/// The section's own state: its fields (one per key and per setting), what went wrong.
#[derive(Default)]
pub struct PacksState {
    fields: HashMap<String, Entity<InputState>>,
    error: Option<String>,
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
        cx.notify();
    });
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
            cx.refresh_windows();
        });
    })
    .detach();
}

/// The section.
pub fn render(page: &WeakEntity<SettingsPage>, window: &mut Window, cx: &mut App) -> AnyElement {
    let Some(entity) = page.upgrade() else { return div().into_any_element() };
    let theme = cx.theme();
    let (fg, muted, border, danger, green) = (theme.foreground, theme.muted_foreground, theme.border, theme.danger, theme.green);
    let installed = packs::dir().map(|d| packs::installed(&d)).unwrap_or_default();
    let settings = cx.global::<crate::AppSettings>().0.borrow().clone();
    let mut list = div().flex().flex_col().gap_2();
    if installed.is_empty() {
        list = list.child(div().text_sm().text_color(muted).child("No packs yet. A pack is a folder with a pack.json and its scripts."));
    }
    for pack in installed {
        list = list.child(card(page, &entity, &pack, &settings, window, cx));
    }
    let error = entity.read(cx).packs.error.clone();
    let p_install = page.clone();
    let _ = (fg, border, green);
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().pt_4().font_weight(FontWeight::SEMIBOLD).text_color(cx.theme().foreground).child("Packs"))
        .child(div().text_sm().text_color(muted).child(
            "Ready-made tools for every agent, run inside Keleuo (nothing to install): each can reach only the services it names and read only its own keys, and writes only in the session's folder.",
        ))
        .child(list)
        .child(div().child(Button::new("pack-install").small().outline().icon(IconName::Plus).label("Install from folder…").on_click(move |_, _, cx| install(p_install.clone(), cx))))
        .children(error.map(|e| div().text_sm().text_color(danger).child(e)))
        .into_any_element()
}

fn card(page: &WeakEntity<SettingsPage>, entity: &Entity<SettingsPage>, pack: &Pack, settings: &keleuo_core::settings::Settings, window: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let (fg, muted, border, green) = (theme.foreground, theme.muted_foreground, theme.border, theme.green);
    let m = &pack.manifest;
    let on = !settings.packs_off.contains(&m.id);
    let has = |name: &str| keleuo_core::secrets::get(&pack.secret_account(name)).is_some();
    let (id1, id2) = (m.id.clone(), m.id.clone());
    let dir = pack.dir.clone();
    let mut tools = div().flex().flex_col().gap_0p5();
    for t in &m.tools {
        let ready = packs::Pack::ready(t, &has);
        let mut line = format!("{}: {}", t.name, t.description);
        if !t.cost.is_empty() {
            line.push_str(&format!(" ({})", t.cost));
        }
        tools = tools.child(
            div()
                .flex()
                .gap_2()
                .text_sm()
                .child(div().flex_none().text_color(if ready { green } else { muted }).child(if ready { "✓" } else { "○" }))
                .child(div().min_w_0().text_color(if ready { fg } else { muted }).child(line)),
        );
    }
    // Its keys: a field each, saved to the key store (a kept one shows as kept, never its value).
    let mut keys = div().flex().flex_col().gap_1();
    for s in &m.secrets {
        let account = pack.secret_account(&s.name);
        let kept = keleuo_core::secrets::get(&account).is_some();
        let input = entity.update(cx, |p, cx| field(&mut p.packs, &format!("{}/secret/{}", m.id, s.name), if kept { "kept in the key store; type to replace" } else { "paste the key" }, true, "", window, cx));
        let (inp, acct, pg) = (input.clone(), account.clone(), page.clone());
        let label = if s.label.is_empty() { s.name.clone() } else { s.label.clone() };
        keys = keys.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().w(px(170.)).flex_none().text_sm().text_color(fg).child(label))
                .child(div().flex_1().child(Input::new(&input).small()))
                .child(div().w(px(16.)).text_color(green).child(if kept { "✓" } else { "" }))
                .child(Button::new(SharedString::from(format!("pack-key-{account}"))).xsmall().outline().label("Save").on_click(move |_, window, cx| {
                    let value = inp.read(cx).value().trim().to_string();
                    if value.is_empty() {
                        return;
                    }
                    let saved = keleuo_core::secrets::set(&acct, &value).map_err(|e| format!("Could not keep the key: {e}"));
                    inp.update(cx, |i, cx| i.set_value("", window, cx));
                    set_error(&pg, saved.err(), cx);
                    cx.refresh_windows();
                })),
        );
    }
    // Its settings: a field each, saved with Keleuo's settings.
    let mut sets = div().flex().flex_col().gap_1();
    for st in &m.settings {
        let current = settings.pack_settings.get(&m.id).and_then(|s| s.get(&st.name)).cloned().unwrap_or_default();
        let placeholder = if st.placeholder.is_empty() { st.default.clone() } else { st.placeholder.clone() };
        let input = entity.update(cx, |p, cx| field(&mut p.packs, &format!("{}/setting/{}", m.id, st.name), &placeholder, false, &current, window, cx));
        let (inp, pid, name) = (input.clone(), m.id.clone(), st.name.clone());
        let label = if st.label.is_empty() { st.name.clone() } else { st.label.clone() };
        let hint = if st.choices.is_empty() { String::new() } else { st.choices.join(" / ") };
        sets = sets.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().w(px(170.)).flex_none().text_sm().text_color(fg).child(label))
                .child(div().flex_1().child(Input::new(&input).small()))
                .child(div().text_xs().text_color(muted).child(hint))
                .child(Button::new(SharedString::from(format!("pack-set-{pid}-{name}"))).xsmall().ghost().label("Save").on_click(move |_, _, cx| {
                    let value = inp.read(cx).value().trim().to_string();
                    let (pid, name) = (pid.clone(), name.clone());
                    change(cx, move |s| {
                        s.pack_settings.entry(pid).or_default().insert(name, value);
                    });
                })),
        );
    }
    div()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(border)
        .when(!on, |d| d.opacity(0.6))
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(m.name.clone()))
                .child(div().text_xs().text_color(muted).child(format!("{} · {}", m.version, m.id)))
                .child(div().flex_1())
                .child(Button::new(SharedString::from(format!("pack-remove-{}", m.id))).xsmall().ghost().icon(gpui_kit::assets::IconName::Trash).tooltip("Remove").on_click(move |_, _, cx| {
                    let _ = std::fs::remove_dir_all(&dir);
                    let id = id2.clone();
                    change(cx, move |s| s.packs_off.retain(|x| *x != id));
                }))
                .child(Switch::new(SharedString::from(format!("pack-on-{}", m.id))).checked(on).on_click(move |on: &bool, _, cx| {
                    let (id, on) = (id1.clone(), *on);
                    change(cx, move |s| {
                        s.packs_off.retain(|x| *x != id);
                        if !on {
                            s.packs_off.push(id);
                        }
                    });
                })),
        )
        .when(!m.description.is_empty(), |d| d.child(div().text_sm().text_color(muted).child(m.description.clone())))
        .child(tools)
        .when(!m.secrets.is_empty(), |d| d.child(div().pt_1().text_xs().text_color(muted).child("Keys (kept in the system's key store; agents never see them)")).child(keys))
        .when(!m.settings.is_empty(), |d| d.child(div().pt_1().text_xs().text_color(muted).child("Settings")).child(sets))
        .when(!m.network.is_empty(), |d| d.child(div().text_xs().text_color(muted).child(format!("May reach: {}", m.network.join(", ")))))
        .into_any_element()
}
