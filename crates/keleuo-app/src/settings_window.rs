//! The Settings window (Keleuo > Settings…, Cmd+,): every option in one place, a page per
//! subject. Each change is saved at once and applies at once; the menus' checks follow.
//! One window at a time; asking again brings it to the front.

use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings as SettingsView,
};
use gpui_kit::component::{ActiveTheme, IconName};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleuo_core::settings::{Prefs, Settings, TERMINAL_FONT_SIZE, TERMINAL_FONT_SIZES};

use crate::AppSettings;

/// The open Settings window, if any.
#[derive(Default)]
struct Open(Option<AnyWindowHandle>);
impl Global for Open {}

pub struct SettingsWindow;

/// Show the Settings window (or bring the open one forward).
pub fn open(cx: &mut App) {
    if let Some(handle) = cx.try_global::<Open>().and_then(|o| o.0)
        && handle.update(cx, |_, window, _| window.activate_window()).is_ok()
    {
        return;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(760.), px(560.)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some("Settings".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.), px(12.))),
        }),
        is_minimizable: false,
        window_min_size: Some(size(px(560.), px(400.))),
        ..Default::default()
    };
    match gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| SettingsWindow)) {
        Ok((handle, _)) => cx.set_global(Open(Some(handle))),
        Err(e) => eprintln!("keleuo: could not open the Settings window: {e}"),
    }
}

/// Read a setting.
fn get<T>(cx: &App, f: impl FnOnce(&Settings) -> T) -> T {
    f(&cx.global::<AppSettings>().0.borrow())
}

/// Change the settings, save them, and let everything that shows them know: the menus'
/// checks, whatever watches the settings (the Dock badge), and every window.
pub fn change(cx: &mut App, f: impl FnOnce(&mut Settings)) {
    cx.update_global::<AppSettings, _>(|settings, _| {
        let mut s = settings.0.borrow_mut();
        f(&mut s);
        let _ = s.save();
    });
    cx.set_menus(crate::actions::menus(cx));
    cx.refresh_windows();
}

/// A switch for one of the on-by-default prefs.
fn pref(title: &'static str, description: &'static str, field: fn(&mut Prefs) -> &mut bool) -> SettingItem {
    switch_pref(title, description, field, true)
}

fn pref_off_by_default(title: &'static str, description: &'static str, field: fn(&mut Prefs) -> &mut bool) -> SettingItem {
    switch_pref(title, description, field, false)
}

fn switch_pref(title: &'static str, description: &'static str, field: fn(&mut Prefs) -> &mut bool, default: bool) -> SettingItem {
    SettingItem::new(
        title,
        SettingField::switch(
            move |cx: &App| get(cx, |s| *field(&mut s.prefs.clone())),
            move |on: bool, cx: &mut App| change(cx, |s| *field(&mut s.prefs) = on),
        )
        .default_value(default),
    )
    .description(description)
}

/// A choice among a few named values.
fn choice(
    title: &'static str,
    description: &'static str,
    options: &[(&'static str, &'static str)],
    default: &'static str,
    read: fn(&Settings) -> String,
    write: fn(&mut Settings, &str),
) -> SettingItem {
    let options = options.iter().map(|(v, label)| (SharedString::from(*v), SharedString::from(*label))).collect();
    SettingItem::new(
        title,
        SettingField::dropdown(
            options,
            move |cx: &App| {
                let v = get(cx, read);
                SharedString::from(if v.is_empty() { default.to_string() } else { v })
            },
            move |v: SharedString, cx: &mut App| change(cx, |s| write(s, &v)),
        )
        .default_value(default),
    )
    .description(description)
}

fn general() -> SettingPage {
    SettingPage::new("General").icon(IconName::Settings).resettable(true).groups([
        SettingGroup::new().title("Look").item(
            SettingItem::new(
                "Appearance",
                SettingField::dropdown(
                    vec![("system".into(), "System".into()), ("light".into(), "Light".into()), ("dark".into(), "Dark".into())],
                    |cx: &App| {
                        let a = get(cx, |s| s.appearance.clone());
                        SharedString::from(if a.is_empty() { "system".to_string() } else { a })
                    },
                    |v: SharedString, cx: &mut App| {
                        crate::theme::set_appearance(&v, cx);
                        change(cx, |_| {});
                    },
                )
                .default_value("system"),
            )
            .description("Light or dark, or following macOS."),
        ),
        SettingGroup::new().title("Sessions").items([
            choice(
                "New sessions open in",
                "The chat, or the agent's own terminal interface (for agents that have one).",
                &[("chat", "Chat"), ("terminal", "Terminal")],
                "chat",
                |s| s.default_view.clone(),
                |s, v| s.default_view = v.to_string(),
            ),
            pref(
                "Reopen sessions at start",
                "The sessions open when Keleuo quit open again when it starts.",
                |p| &mut p.reopen_sessions,
            ),
        ]),
    ])
}

fn chat() -> SettingPage {
    SettingPage::new("Chat").icon(IconName::Bot).resettable(true).groups([
        SettingGroup::new().title("Conversation").items([
            choice(
                "Permission mode for new sessions",
                "Auto lets Claude run what it judges safe and asks about the rest; Ask before acting asks about every change and command. Each session can switch on its own, and keeps its choice.",
                &[("auto", "Auto"), ("default", "Ask before acting"), ("acceptEdits", "Accept edits"), ("plan", "Plan")],
                "auto",
                |s| s.default_permission_mode.clone(),
                |s, v| s.default_permission_mode = v.to_string(),
            ),
            choice(
                "Show what a session costs",
                "Auto shows the cost only when the agent is billed per use, and how full its context is otherwise.",
                &[("auto", "Auto"), ("always", "Always"), ("never", "Never (context instead)")],
                "auto",
                |s| s.cost_display.clone(),
                |s, v| s.cost_display = v.to_string(),
            ),
            pref(
                "Card of changed files after a turn",
                "A turn that changed files ends with a card listing them, each opening as a diff.",
                |p| &mut p.turn_card,
            ),
            pref(
                "Find files changed by commands",
                "Files a command changed (a formatter, sed, a generator) join a turn's changes, found with git.",
                |p| &mut p.command_changes,
            ),
        ]),
        SettingGroup::new().title("Message box").items([
            pref(
                "Highlight Markdown",
                "Code, headings, bold, italic and links are marked as you type. The text is not changed.",
                |p| &mut p.markdown_marks,
            ),
            SettingItem::new(
                "Monospace while a message has code",
                SettingField::switch(
                    |cx: &App| !get(cx, |s| s.always_proportional),
                    |on: bool, cx: &mut App| change(cx, |s| s.always_proportional = !on),
                )
                .default_value(true),
            )
            .description("The box turns to the code font while the message holds a ``` block."),
            pref(
                "Long pastes as pills",
                "A long paste goes in as one pill, its text kept aside, to show or remove.",
                |p| &mut p.paste_pills,
            ),
        ]),
        SettingGroup::new().title("Experimental").items([pref_off_by_default(
            "Hybrid Chat and Terminal",
            "Claude Code's own interface runs for the whole session and Chat is drawn from it, so switching is instant, even mid-turn. Off: Chat runs Claude Code headless, and switching restarts it in the other view between turns. Applies to sessions as they start.",
            |p| &mut p.hybrid_views,
        )]),
    ])
}

fn terminals() -> SettingPage {
    let (min, max) = (*TERMINAL_FONT_SIZES.start(), *TERMINAL_FONT_SIZES.end());
    SettingPage::new("Terminals").icon(IconName::SquareTerminal).resettable(true).group(
        SettingGroup::new().items([
            SettingItem::new(
                "Font size",
                SettingField::number_input(
                    NumberFieldOptions { min: min as f64, max: max as f64, step: 0.5 },
                    |cx: &App| get(cx, |s| s.prefs.terminal_font_size()) as f64,
                    move |v: f64, cx: &mut App| change(cx, |s| s.prefs.terminal_font_size = (v as f32).clamp(min, max)),
                )
                .default_value(TERMINAL_FONT_SIZE as f64),
            )
            .description("In points, for the shell terminals and the agents' own interfaces."),
            pref(
                "Keep terminal history",
                "A terminal shows what it showed before Keleuo quit. Kept 30 days.",
                |p| &mut p.keep_terminal_history,
            ),
        ]),
    )
}

fn notifications() -> SettingPage {
    SettingPage::new("Notifications").icon(IconName::Bell).resettable(true).group(
        SettingGroup::new().title("The Dock").description("A bounce, and a count on Keleuo's icon, while the window is not in front.").items([
            SettingItem::new(
                "When a session asks",
                SettingField::switch(
                    |cx: &App| get(cx, |s| s.notify.waiting),
                    |on: bool, cx: &mut App| change(cx, |s| s.notify.waiting = on),
                )
                .default_value(true),
            )
            .description("It waits for a permission answer, or asks something."),
            SettingItem::new(
                "When a turn finishes",
                SettingField::switch(
                    |cx: &App| get(cx, |s| s.notify.finished),
                    |on: bool, cx: &mut App| change(cx, |s| s.notify.finished = on),
                )
                .default_value(true),
            )
            .description("It is done and waits for the next message."),
        ]),
    )
}

impl Render for SettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // Room for the window buttons above the page.
            .pt(px(32.))
            .child(
                SettingsView::new("keleuo-settings")
                    .sidebar_width(px(200.))
                    .pages([general(), chat(), terminals(), notifications()]),
            )
    }
}
