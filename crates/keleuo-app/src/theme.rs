//! The look, light and dark: distinct surface tones so each region reads as itself (sidebar,
//! Claude, then the editor), clear dividers, an obvious active tab and quiet inactive ones. The
//! brand accent is the icon's periwinkle; crane yellow marks what wants attention. It follows
//! the system's appearance unless the setting (`appearance`: system, light or dark) says
//! otherwise, and changes live.

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

/// The icon's periwinkle.
/// The interface text's weight: between regular (400) and medium (500).
pub const UI_WEIGHT: f32 = 430.;

pub const PERIWINKLE: u32 = 0x6084e8;
/// The crane's yellow.
pub const CRANE: u32 = 0xffc354;

/// The light lists' ground with the pressed-in look: a shade darker, for the text's white edge.
const PRESSED_GROUND: u32 = 0xe6e6e9;
/// And its text.
const PRESSED_TEXT: u32 = 0x45454b;

thread_local! {
    /// The light lists' ground, while trying others (the control channel's `sidebar`).
    static SIDEBAR_TONE: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

/// Try `tone` as the light lists' ground (`None`: its own).
pub fn try_sidebar_tone(tone: Option<u32>, cx: &mut App) {
    SIDEBAR_TONE.with(|t| t.set(tone));
    apply(wants_dark(cx), cx);
    cx.refresh_windows();
}

/// The side lists' main text: a dark grey with the pressed-in look on a light theme (black
/// reads as printed on, not pressed in), else the theme's.
pub fn list_text(cx: &App) -> Hsla {
    if crate::prefs(cx).inset_text && !cx.theme().is_dark() { hex(PRESSED_TEXT) } else { cx.theme().foreground }
}

/// `text` pressed into the surface, letterpress style: a faint copy a pixel lower drawn
/// under it (light on a light theme, dark on a dark one), when the setting is on. `el` holds it
/// and sets its size, colour and truncation.
pub fn pressed(el: Div, text: impl Into<SharedString>, cx: &App) -> Div {
    let text = text.into();
    if !crate::prefs(cx).inset_text {
        return el.child(text);
    }
    let shadow = if cx.theme().is_dark() { gpui::black().opacity(0.45) } else { gpui::white().opacity(0.75) };
    el.relative()
        .child(div().absolute().top(px(1.)).left_0().right_0().truncate().text_color(shadow).child(text.clone()))
        .child(div().relative().truncate().child(text))
}

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
    crate::fonts::apply(cx);
    crate::palette::set_dark(dark);
    // The pressed-in look: the whole light sidebar a shade darker, for the text's white edge.
    let pressed = crate::prefs(cx).inset_text;
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
            c.sidebar = hex(if pressed { PRESSED_GROUND } else { 0xf1f1f1 });
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

/// The ground of the tool panels (files, changes, skills, the agent): a much lighter grey than
/// the session list's, so the chat and files stand out white beside them.
pub fn tool_ground(cx: &App) -> Hsla {
    let t = cx.theme();
    if !t.is_dark() {
        let tried = SIDEBAR_TONE.with(|t| t.get());
        if let Some(tone) = tried.or(crate::prefs(cx).inset_text.then_some(PRESSED_GROUND)) {
            return hex(tone);
        }
    }
    crate::sheet_tabs::mix(t.background, t.sidebar, 0.45)
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
