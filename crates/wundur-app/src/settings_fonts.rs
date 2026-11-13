//! Settings > General > Fonts: the app's, the chat's and the terminals' font, each the
//! system's or any Google Font, found by typing its name (see [`crate::fonts`]).

use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::fonts::{self, Which};
use crate::settings_window::SettingsPage;

/// Families listed under the search box, at most.
const SHOWN: usize = 12;

/// The open font search: for which setting, its box.
pub struct Search {
    which: Which,
    input: Entity<InputState>,
    _changed: Subscription,
}

fn default_name(which: Which) -> &'static str {
    match which {
        Which::App => "System (SF Pro)",
        Which::Chat => "Same as the app",
        Which::Terminal => "System (Menlo)",
    }
}

fn open(page: &WeakEntity<SettingsPage>, which: Which, window: &mut Window, cx: &mut App) {
    fonts::ask_catalogue(cx);
    let _ = page.update(cx, |p, cx| {
        if p.font_search.as_ref().is_some_and(|s| s.which == which) {
            p.font_search = None;
        } else {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search Google Fonts"));
            input.update(cx, |i, cx| i.focus(window, cx));
            let changed = cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify());
            p.font_search = Some(Search { which, input, _changed: changed });
        }
        cx.notify();
    });
}

/// Open `which`'s search with `text` typed in (the control channel's `font-search`).
pub fn open_for_test(page: &WeakEntity<SettingsPage>, which: Which, text: &str, window: &mut Window, cx: &mut App) {
    open(page, which, window, cx);
    if let Some(input) = page.upgrade().and_then(|p| p.read(cx).font_search.as_ref().map(|s| s.input.clone())) {
        input.update(cx, |i, cx| i.set_value(text.to_string(), window, cx));
    }
}

fn close(page: &WeakEntity<SettingsPage>, cx: &mut App) {
    let _ = page.update(cx, |p, cx| {
        p.font_search = None;
        cx.notify();
    });
}

/// One font's row, and its search when open.
pub fn render(page: &WeakEntity<SettingsPage>, which: Which, cx: &mut App) -> AnyElement {
    let Some(entity) = page.upgrade() else { return div().into_any_element() };
    let theme = cx.theme();
    let (fg, muted, hover, danger) = (theme.foreground, theme.muted_foreground, theme.secondary_hover, theme.danger);
    let (title, description) = match which {
        Which::App => ("App font", "Panels, lists, inputs and menus."),
        Which::Chat => ("Chat font", "The conversation and the message box."),
        Which::Terminal => ("Terminal font", "Terminals, the agents' own interfaces, and code, commands and diffs in the chat."),
    };
    let settings = cx.global::<crate::AppSettings>().0.borrow().clone();
    let current = match which {
        Which::App => settings.ui_font,
        Which::Chat => settings.chat_font,
        Which::Terminal => settings.terminal_font,
    };
    let state = cx.global::<fonts::Fonts>();
    let fetching = state.fetching.as_ref().filter(|(w, _)| *w == which).map(|(_, n)| n.clone());
    let error = state.error.clone();
    let label = match (&fetching, current.is_empty()) {
        (Some(n), _) => format!("Fetching {n}…"),
        (None, true) => default_name(which).to_string(),
        (None, false) => current.clone(),
    };
    let shown_in = |name: &str, cx: &App| -> SharedString {
        if fonts::is_loaded(name, cx) {
            name.to_string().into()
        } else if which == Which::Terminal {
            fonts::code(cx)
        } else {
            fonts::ui(cx)
        }
    };
    let p_open = page.clone();
    let row = div()
        .flex()
        .items_center()
        .gap_4()
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_sm().text_color(fg).child(title))
                .child(div().text_xs().text_color(muted).child(description)),
        )
        .child(
            Button::new(SharedString::from(format!("font-{which:?}")))
                .small()
                .outline()
                .dropdown_caret(true)
                .child(div().font_family(shown_in(&current, cx)).child(label))
                .on_click(move |_, window, cx| open(&p_open, which, window, cx)),
        );
    let search = entity.read(cx).font_search.as_ref().filter(|s| s.which == which).map(|s| s.input.clone());
    let mut col = div().flex().flex_col().gap_2().py_1().child(row);
    if let Some(input) = search {
        let query = input.read(cx).value().to_string();
        let mut list = div().flex().flex_col();
        let item = |id: String, name: String, note: String, font: SharedString, on: bool, pick: Box<dyn Fn(&mut App)>| {
            div()
                .id(SharedString::from(id))
                .px_2()
                .py_1()
                .rounded_sm()
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .hover(|d| d.bg(hover))
                .child(div().w(px(14.)).text_color(fg).child(if on { "✓" } else { "" }))
                .child(div().flex_1().truncate().font_family(font).text_color(fg).child(name))
                .child(div().text_xs().text_color(muted).child(note))
                .on_click(move |_, _, cx| pick(cx))
        };
        let (p, w) = (page.clone(), which);
        list = list.child(item(format!("font-{which:?}-default"), default_name(which).into(), String::new(), fonts::ui(cx), current.is_empty(), Box::new(move |cx| {
            close(&p, cx);
            fonts::pick(w, None, cx);
        })));
        match cx.global::<fonts::Fonts>().catalogue.clone() {
            None => list = list.child(div().px_2().py_1().text_xs().text_color(muted).child("Asking Google Fonts…")),
            Some(Err(e)) => list = list.child(div().px_2().py_1().text_xs().text_color(danger).child(e)),
            Some(Ok(_)) => {
                let found = fonts::matches(which, &query, SHOWN, cx);
                if found.is_empty() {
                    list = list.child(div().px_2().py_1().text_xs().text_color(muted).child("No Google Font by that name."));
                }
                for f in found {
                    let here = fonts::is_loaded(&f.name, cx);
                    let note = if here { format!("{} · fetched", f.category) } else { f.category.clone() };
                    let (p, fam) = (page.clone(), f.clone());
                    list = list.child(item(format!("font-{which:?}-{}", f.name), f.name.clone(), note, shown_in(&f.name, cx), f.name == current, Box::new(move |cx| {
                        close(&p, cx);
                        fonts::pick(which, Some(fam.clone()), cx);
                    })));
                }
            }
        }
        col = col.child(
            div()
                .ml_auto()
                .w(px(420.))
                .flex()
                .flex_col()
                .gap_1()
                .child(Input::new(&input).small())
                .child(list)
                .child(div().px_2().text_xs().text_color(muted).child("Fonts you pick are fetched once from Google Fonts and kept; they're all free to use.")),
        );
    }
    if let Some((_, e)) = error.filter(|(w, _)| *w == which) {
        col = col.child(div().text_xs().text_color(danger).child(e));
    }
    col.into_any_element()
}
