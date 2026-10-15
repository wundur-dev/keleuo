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
            // Neutral greys: the ground (the session list, the space between panels) a soft
            // grey, the panels white on it.
            c.sidebar = hex(0xf1f1f1);
            c.tab_bar = hex(0xe9e9e9);
            c.background = hex(0xffffff);
            c.secondary = hex(0xf3f3f3);
            c.secondary_hover = hex(0xe8e8e8);
            c.border = hex(0xe1e1e1);
            c.tab_active = hex(0xffffff);
            c.tab_active_foreground = hex(0x16181c);
            c.tab_foreground = hex(0x5d5d5d);
            c.muted_foreground = hex(0x6b6b6b);
            c.foreground = hex(0x1a1a1a);
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
        // The title bar is chrome, the grey the work area sits in, with no line under it.
        c.title_bar = c.sidebar;
        c.title_bar_border = c.sidebar;
    });
}

/// Match the system (or the setting) if it changed; redraw everything when it does.
/// The ground of tab strips and title bars inside a panel: between the panel's white and the
/// grey the panels sit on, so a panel's edges stay its own.
pub fn strip_ground(cx: &App) -> Hsla {
    let t = cx.theme();
    crate::sheet_tabs::mix(t.background, t.sidebar, 0.55)
}

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
