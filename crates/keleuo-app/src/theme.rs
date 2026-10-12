//! The look, light and dark: distinct surface tones so each region reads as itself (sidebar,
//! Claude, then the editor), clear dividers, an obvious active tab and quiet inactive ones. The
//! brand accent is the icon's periwinkle; crane yellow marks what wants attention. It follows
//! the system's appearance unless the setting (`appearance`: system, light or dark) says
//! otherwise, and changes live.

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

/// The icon's periwinkle.
pub const PERIWINKLE: u32 = 0x6084e8;
/// The crane's yellow.
pub const CRANE: u32 = 0xffc354;

fn hex(c: u32) -> Hsla {
    rgb(c).into()
}

/// Whether the app should be dark now: the setting, else the system.
pub fn wants_dark(cx: &App) -> bool {
    let setting = cx.global::<crate::AppSettings>().0.borrow().appearance.clone();
    match setting.as_str() {
        "light" => false,
        "dark" => true,
        _ => matches!(cx.window_appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark),
    }
}

/// Apply the light or dark theme to everything, the terminals' palette included.
pub fn apply(dark: bool, cx: &mut App) {
    Theme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
    crate::palette::set_dark(dark);
    Theme::update(cx, |t| {
        let c = &mut t.colors;
        if dark {
            // Surfaces, darkest to lightest: sidebar, tab bars, Claude, editor.
            c.sidebar = hex(0x111214);
            c.tab_bar = hex(0x16171a);
            c.background = hex(0x1c1d21);
            c.secondary = hex(0x25272c);
            c.secondary_hover = hex(0x2d3036);
            // Lines between regions: visible but quiet.
            c.border = hex(0x30333a);
            // The active tab is lifted and bright; inactive tabs recede.
            c.tab_active = hex(0x25272c);
            c.tab_active_foreground = hex(0xf2f3f5);
            c.tab_foreground = hex(0x8b8f98);
            c.muted_foreground = hex(0x8f939c);
            c.foreground = hex(0xdfe1e5);
            c.accent = hex(0x2b3963);
            c.ring = hex(PERIWINKLE);
            c.blue = hex(0x8aa4f2);
            c.warning = hex(CRANE);
        } else {
            c.sidebar = hex(0xedeff3);
            c.tab_bar = hex(0xe6e9ee);
            c.background = hex(0xffffff);
            c.secondary = hex(0xf1f3f6);
            c.secondary_hover = hex(0xe5e8ee);
            c.border = hex(0xd5d9e0);
            c.tab_active = hex(0xffffff);
            c.tab_active_foreground = hex(0x16181c);
            c.tab_foreground = hex(0x5a5f69);
            c.muted_foreground = hex(0x5a5f69);
            c.foreground = hex(0x1c1e22);
            c.accent = hex(0xdfe6fb);
            c.ring = hex(PERIWINKLE);
            // Text colours that keep 4.5:1 on white.
            c.blue = hex(0x3a5fcf);
            c.green = hex(0x1d7a3b);
            c.red = hex(0xc0392b);
            c.magenta = hex(0x8e44ad);
            c.cyan = hex(0x13798a);
            c.warning = hex(0x9a6400);
        }
    });
}

/// Match the system (or the setting) if it changed; redraw everything when it does.
pub fn sync(cx: &mut App) {
    let dark = wants_dark(cx);
    if cx.theme().is_dark() != dark {
        apply(dark, cx);
        cx.refresh_windows();
    }
}

/// System, Light or Dark: save it, and set the window chrome to match.
pub fn set_appearance(choice: &str, cx: &mut App) {
    let settings = cx.global::<crate::AppSettings>().0.clone();
    settings.borrow_mut().appearance = choice.to_string();
    let _ = settings.borrow().save();
    cx.set_window_appearance(match choice {
        "light" => Some(WindowAppearance::Light),
        "dark" => Some(WindowAppearance::Dark),
        _ => None,
    });
    sync(cx);
    // `sync` compares with the theme; apply anyway so the chrome and theme agree.
    let dark = wants_dark(cx);
    apply(dark, cx);
    cx.refresh_windows();
}

use gpui_kit::component::ActiveTheme;
