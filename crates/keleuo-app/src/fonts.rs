//! The fonts: the app's, the chat's and the terminals' (which code in the chat uses too), the
//! settings `ui_font`, `chat_font` and `terminal_font`. Each is the system's or any Google Font
//! ([`keleuo_core::google_fonts`]): picking one fetches it (once; it is kept) and puts it on
//! live.

use std::borrow::Cow;
use std::collections::HashSet;

use gpui_kit::component::Theme;
use gpui_kit::*;
use keleuo_core::google_fonts::{self, Family};

/// The system's interface font.
pub const SYSTEM_SANS: &str = ".SystemUIFont";

/// Which font a setting is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    App,
    Chat,
    Terminal,
}

#[derive(Default)]
pub struct Fonts {
    /// Families the text system has.
    loaded: HashSet<String>,
    /// Google's catalogue, once asked for.
    pub catalogue: Option<Result<Vec<Family>, String>>,
    asking: bool,
    /// The family being fetched, for which setting.
    pub fetching: Option<(Which, String)>,
    /// Why the last fetch failed, for which setting.
    pub error: Option<(Which, String)>,
}

impl Global for Fonts {}

/// Give the text system the fonts fetched before.
pub fn register(cx: &mut App) {
    cx.set_global(Fonts::default());
    let Some(dir) = google_fonts::dir() else { return };
    for (family, files) in google_fonts::fetched(&dir) {
        load(&family, &files, cx);
    }
}

fn load(family: &str, files: &[std::path::PathBuf], cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> = files.iter().filter_map(|f| std::fs::read(f).ok()).map(Cow::Owned).collect();
    match cx.text_system().add_fonts(fonts) {
        Ok(()) => {
            cx.global_mut::<Fonts>().loaded.insert(family.to_string());
        }
        Err(e) => eprintln!("keleuo: could not load {family}: {e}"),
    }
}

fn setting(s: &keleuo_core::settings::Settings, which: Which) -> &String {
    match which {
        Which::App => &s.ui_font,
        Which::Chat => &s.chat_font,
        Which::Terminal => &s.terminal_font,
    }
}

/// The family a setting names, when the text system has it.
fn chosen(which: Which, cx: &App) -> Option<SharedString> {
    let name = setting(&cx.global::<crate::AppSettings>().0.borrow(), which).clone();
    cx.try_global::<Fonts>().is_some_and(|f| f.loaded.contains(&name)).then(|| name.into())
}

/// The app's font now.
pub fn ui(cx: &App) -> SharedString {
    chosen(Which::App, cx).unwrap_or(SYSTEM_SANS.into())
}

/// The chat's font now (the app's unless it has its own).
pub fn chat(cx: &App) -> SharedString {
    chosen(Which::Chat, cx).unwrap_or_else(|| ui(cx))
}

/// The terminals' font now, and code's in the chat.
pub fn code(cx: &App) -> SharedString {
    chosen(Which::Terminal, cx).unwrap_or(crate::terminal_view::FONT_FAMILY.into())
}

/// Text's weight in `family`: the system font a touch heavier than regular (as
/// [`crate::theme::UI_WEIGHT`]); others, which come in whole weights, regular.
pub fn weight(family: &SharedString) -> FontWeight {
    if family == SYSTEM_SANS { FontWeight(crate::theme::UI_WEIGHT) } else { FontWeight::NORMAL }
}

/// Put the chosen fonts on the theme (after a theme change too, which resets them).
pub fn apply(cx: &mut App) {
    let (ui, code) = (ui(cx), code(cx));
    Theme::update(cx, |t| {
        t.font_family = ui;
        t.mono_font_family = code;
    });
}

/// Ask for Google's catalogue, once.
pub fn ask_catalogue(cx: &mut App) {
    let f = cx.global_mut::<Fonts>();
    if f.asking || f.catalogue.as_ref().is_some_and(Result::is_ok) {
        return;
    }
    f.asking = true;
    cx.spawn(async move |cx: &mut AsyncApp| {
        let answer = cx.background_executor().spawn(async { google_fonts::catalogue().map_err(|e| format!("{e:#}")) }).await;
        cx.update(|cx| {
            let f = cx.global_mut::<Fonts>();
            f.catalogue = Some(answer);
            f.asking = false;
            cx.refresh_windows();
        });
    })
    .detach();
}

fn set(which: Which, name: &str, cx: &mut App) {
    let name = name.to_string();
    crate::settings_window::change(cx, move |s| match which {
        Which::App => s.ui_font = name,
        Which::Chat => s.chat_font = name,
        Which::Terminal => s.terminal_font = name,
    });
    apply(cx);
    cx.refresh_windows();
}

/// Use `family` (`None`: the default) for `which`: fetched first when it is not here yet.
pub fn pick(which: Which, family: Option<Family>, cx: &mut App) {
    let Some(family) = family else { return set(which, "", cx) };
    cx.global_mut::<Fonts>().error = None;
    if cx.global::<Fonts>().loaded.contains(&family.name) {
        return set(which, &family.name, cx);
    }
    let Some(dir) = google_fonts::dir() else { return };
    cx.global_mut::<Fonts>().fetching = Some((which, family.name.clone()));
    cx.refresh_windows();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let name = family.name.clone();
        let got = cx.background_executor().spawn(async move { google_fonts::fetch(&family, &dir).map_err(|e| format!("{e:#}")) }).await;
        cx.update(|cx| {
            cx.global_mut::<Fonts>().fetching = None;
            match got {
                Ok(files) => {
                    load(&name, &files, cx);
                    set(which, &name, cx);
                }
                Err(e) => {
                    cx.global_mut::<Fonts>().error = Some((which, format!("Could not fetch {name}: {e}")));
                    cx.refresh_windows();
                }
            }
        });
    })
    .detach();
}

/// The catalogue's families for `which` matching `query` (monospace ones for the terminals),
/// most used first, at most `limit`.
pub fn matches(which: Which, query: &str, limit: usize, cx: &App) -> Vec<Family> {
    let Some(Ok(list)) = cx.try_global::<Fonts>().and_then(|f| f.catalogue.as_ref()) else { return Vec::new() };
    let q = query.trim().to_lowercase();
    let mut found: Vec<&Family> = list.iter().filter(|f| (which != Which::Terminal || f.is_mono()) && f.name.to_lowercase().contains(&q)).collect();
    // A name that starts with what was typed comes first.
    found.sort_by_key(|f| !f.name.to_lowercase().starts_with(&q));
    found.into_iter().take(limit).cloned().collect()
}

/// Whether the text system has `family`.
pub fn is_loaded(family: &str, cx: &App) -> bool {
    cx.try_global::<Fonts>().is_some_and(|f| f.loaded.contains(family))
}
