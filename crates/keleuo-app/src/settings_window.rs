//! The Settings window (Keleuo > Settings…, Cmd+,): every option in one place, a page per
//! subject. Each change is saved at once and applies at once; the menus' checks follow.
//! One window at a time; asking again brings it to the front.

use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings as SettingsView,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, IconName, Sizable};
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
        ).item(choice(
            "Tab style",
            "How tabs look: square blocks, flat labels with a line under the shown one, or slanted sheets.",
            &[("block", "Blocks"), ("underline", "Underline"), ("sheet", "Sheets")],
            "block",
            |s| s.tab_style.clone(),
            |s, v| {
                s.tab_style = v.to_string();
                crate::sheet_tabs::set_style(v);
            },
        )),
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

/// The models the summary server offers, as last asked (for the model list).
#[derive(Default)]
struct ServerModels {
    /// The address and key they were asked with.
    asked: (String, String),
    /// `None` while asking.
    answer: Option<Result<Vec<String>, String>>,
}
impl Global for ServerModels {}

/// Ask the summary server for its models when its address or key changed since last asked.
fn refresh_models(cx: &mut Context<SettingsWindow>) {
    let asked = get(cx, |s| (s.summaries.endpoint.trim().to_string(), s.summaries.api_key.trim().to_string()));
    if cx.try_global::<ServerModels>().is_some_and(|m| m.asked == asked) || asked.0.is_empty() {
        return;
    }
    cx.set_global(ServerModels { asked: asked.clone(), answer: None });
    cx.spawn(async move |_, cx: &mut AsyncApp| {
        let (endpoint, key) = asked.clone();
        let answer = cx
            .background_executor()
            .spawn(async move { keleuo_core::summary::models(&endpoint, &key).map_err(|e| format!("{e:#}")) })
            .await;
        cx.update(|cx| {
            if cx.global::<ServerModels>().asked == asked {
                cx.set_global(ServerModels { asked, answer: Some(answer) });
                cx.refresh_windows();
            }
        });
    })
    .detach();
}

/// The server's key: masked, with a button to show it.
struct KeyField {
    input: Entity<InputState>,
    _changed: Subscription,
}

fn api_key_field() -> SettingField<SharedString> {
    SettingField::render(|_, window: &mut Window, cx: &mut App| {
        let field = window.use_keyed_state("summary-api-key", cx, |window, cx| {
            let key = get(cx, |s| s.summaries.api_key.clone());
            let input = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("None").default_value(key));
            let _changed = cx.subscribe(&input, |_, input, ev: &InputEvent, cx| {
                if let InputEvent::Change = ev {
                    let key = input.read(cx).value().trim().to_string();
                    change(cx, |s| s.summaries.api_key = key);
                }
            });
            KeyField { input, _changed }
        });
        let input = field.read(cx).input.clone();
        div().w(px(240.)).child(Input::new(&input).mask_toggle())
    })
}

/// Open the summary prompt in the window's editor (the shown session's, when it is on this
/// machine), else in the system's editor for Markdown.
fn edit_prompt(cx: &mut App) {
    let path = match keleuo_core::summary::ensure_prompt_file() {
        Ok(p) => p,
        Err(e) => return eprintln!("keleuo: could not write the summary prompt: {e}"),
    };
    let target = cx.global::<crate::control::ControlTargets>().workspace.clone();
    let opened = target.is_some_and(|(handle, shell)| {
        handle
            .update(cx, |_, window, cx| {
                let ws = shell.upgrade().and_then(|s| s.read(cx).active_workspace());
                match ws {
                    Some(w) if !w.read(cx).host().is_remote() => {
                        // At a line: Markdown opens in the editor rather than rendered.
                        w.update(cx, |w, cx| w.open(path.clone(), Some(1), false, true, window, cx));
                        window.activate_window();
                        true
                    }
                    _ => false,
                }
            })
            .unwrap_or(false)
    });
    if !opened {
        let _ = std::process::Command::new(if cfg!(windows) { "notepad" } else { "open" }).arg(&path).spawn();
    }
}

fn summaries() -> SettingPage {
    let summary_switch = |title: &'static str, description: &'static str, field: fn(&mut keleuo_core::summary::Config) -> &mut bool, default: bool| {
        SettingItem::new(
            title,
            SettingField::switch(
                move |cx: &App| get(cx, |s| *field(&mut s.summaries.clone())),
                move |on: bool, cx: &mut App| change(cx, |s| *field(&mut s.summaries) = on),
            )
            .default_value(default),
        )
        .description(description)
    };
    SettingPage::new("Summaries").icon(IconName::BookOpen).resettable(true).groups([
        SettingGroup::new()
            .title("Turn summaries")
            .description("When a turn ends, a language model you run (LM Studio, Ollama, llama.cpp: any server with the OpenAI API) sums it up: how it ended, what changed, how it was checked and what waits on you. It shows at the end of the turn and in the session list. Only this server gets anything.")
            .items([
                summary_switch("Summarize turns", "Ask the server below after each turn.", |c| &mut c.enabled, false),
                SettingItem::new(
                    "Server",
                    SettingField::input(
                        |cx: &App| get(cx, |s| SharedString::from(s.summaries.endpoint.clone())),
                        |v: SharedString, cx: &mut App| change(cx, |s| s.summaries.endpoint = v.trim().to_string()),
                    )
                    .default_value("http://localhost:1234"),
                )
                .description("Its address, such as http://192.168.1.20:1234 (LM Studio) or http://localhost:11434 (Ollama)."),
                SettingItem::new("Key", api_key_field())
                    .description("For servers that want one (sent as a bearer token). Kept in Keleuo's settings file."),
                SettingItem::new(
                    "Model",
                    SettingField::render(|_, _, cx: &mut App| {
                        let theme = cx.theme();
                        let (muted, red) = (theme.muted_foreground, theme.red);
                        let current = get(cx, |s| s.summaries.model.clone());
                        match cx.try_global::<ServerModels>().and_then(|m| m.answer.clone()) {
                            None => div().text_xs().text_color(muted).child("Asking the server…").into_any_element(),
                            Some(Err(e)) => div().max_w(px(260.)).text_xs().text_color(red).child(format!("Can't reach it: {e}")).into_any_element(),
                            Some(Ok(models)) if models.is_empty() => {
                                div().text_xs().text_color(muted).child("The server has no model loaded").into_any_element()
                            }
                            Some(Ok(models)) => {
                                let label = if current.is_empty() { "Choose a model".to_string() } else { current.clone() };
                                Button::new("summary-model")
                                    .label(label)
                                    .small()
                                    .outline()
                                    .dropdown_caret(true)
                                    .dropdown_menu(move |mut m, _, _| {
                                        for id in &models {
                                            let pick = id.clone();
                                            m = m.item(PopupMenuItem::new(id.clone()).checked(*id == current).on_click(
                                                move |_, _, cx| change(cx, |s| s.summaries.model = pick.clone()),
                                            ));
                                        }
                                        m
                                    })
                                    .into_any_element()
                            }
                        }
                    }),
                )
                .description("The models the server has; one that runs fast is plenty."),
            ]),
        SettingGroup::new().title("What it gets").items([
            summary_switch("The changed files' paths", "Paths only, never their contents. Keeps the Changed section true to what happened.", |c| &mut c.include_files, true),
            summary_switch("A line per tool call", "Each call's tool and what it was about (a command, a file name), never its output.", |c| &mut c.include_tools, false),
            summary_switch("Let the model think first", "For models that reason before answering: slower (seconds become half a minute), sometimes better.", |c| &mut c.thinking, false),
        ]),
        SettingGroup::new().title("Prompt").item(
            SettingItem::new(
                "Instructions",
                SettingField::render(|_, _, _| {
                    div()
                        .flex()
                        .gap_2()
                        .child(Button::new("summary-prompt-edit").label("Edit prompt").small().outline().on_click(|_, _, cx| edit_prompt(cx)))
                        .child(Button::new("summary-prompt-reset").label("Reset").small().ghost().on_click(|_, _, _| {
                            let _ = keleuo_core::summary::reset_prompt();
                        }))
                }),
            )
            .description("What the model is told, as a Markdown file you can change. The summary keeps its fields (status, headline, changed, checked, needs you) whatever it says."),
        ),
    ])
}

impl Render for SettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        refresh_models(cx);
        div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // Room for the window buttons above the page.
            .pt(px(32.))
            .child(
                SettingsView::new("keleuo-settings")
                    .sidebar_width(px(200.))
                    .pages([general(), chat(), terminals(), notifications(), summaries()]),
            )
    }
}
