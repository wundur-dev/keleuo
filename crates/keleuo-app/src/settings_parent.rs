//! Settings > Parent mode ([`keleuo_core::parent`]): on or off, the child's name, where alerts
//! go (an ntfy topic, with a test), the agent's instructions, how many times a day before an
//! alert; a password that locks the page.

use std::collections::HashMap;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::settings_window::{SettingsPage, change};

/// The page's own state: unlocked (for this visit), its fields, what the last action said.
#[derive(Default)]
pub struct ParentState {
    unlocked: bool,
    fields: HashMap<&'static str, Entity<InputState>>,
    said: Option<(bool, String)>,
}

fn field(page: &Entity<SettingsPage>, key: &'static str, placeholder: &str, value: &str, masked: bool, window: &mut Window, cx: &mut App) -> Entity<InputState> {
    let (placeholder, value) = (placeholder.to_string(), value.to_string());
    page.update(cx, |p, cx| {
        p.parent
            .fields
            .entry(key)
            .or_insert_with(|| cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).masked(masked).default_value(value)))
            .clone()
    })
}

fn say(page: &WeakEntity<SettingsPage>, ok: bool, text: impl Into<String>, cx: &mut App) {
    let text = text.into();
    let _ = page.update(cx, |p, cx| {
        p.parent.said = Some((ok, text));
        cx.notify();
    });
}

fn value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().trim().to_string()
}

/// The page.
pub fn render(page: &WeakEntity<SettingsPage>, window: &mut Window, cx: &mut App) -> AnyElement {
    let Some(entity) = page.upgrade() else { return div().into_any_element() };
    let theme = cx.theme();
    let (fg, muted, danger, green, border) = (theme.foreground, theme.muted_foreground, theme.danger, theme.green, theme.border);
    let (config, model) = {
        let s = cx.global::<crate::AppSettings>().0.borrow();
        (s.parent.clone(), s.summaries.model.trim().to_string())
    };
    let state = &entity.read(cx).parent;
    let (unlocked, said) = (state.unlocked || config.password.is_empty(), state.said.clone());
    let heading = |t: &str| div().pt_3().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(t.to_string());
    let note = |t: &str| div().text_sm().text_color(muted).child(t.to_string());
    let row = |label: &str| div().flex().items_center().gap_2().child(div().w(px(170.)).flex_none().text_sm().text_color(fg).child(label.to_string()));
    let mut body = div().w_full().flex().flex_col().gap_2().child(note(
        "For a child who builds things with an agent. The agent is told to stay a helpful tool, and you get an alert when a pattern appears: treating the AI as a friend, chatting instead of making, or at once, signs of distress or personal details shared. You never see what was said: an alert says what kind of thing, how often and when.",
    ));

    // Locked: the password first.
    if !unlocked {
        let pw = field(&entity, "unlock", "Password", "", true, window, cx);
        let (p, i) = (page.clone(), pw.clone());
        body = body.child(heading("Locked")).child(note("Parent mode's settings need the password.")).child(
            row("Password").child(div().flex_1().child(Input::new(&pw).small())).child(Button::new("parent-unlock").small().primary().label("Unlock").on_click(move |_, window, cx| {
                let typed = i.read(cx).value().to_string();
                let ok = cx.global::<crate::AppSettings>().0.borrow().parent.unlocks(&typed);
                i.update(cx, |i, cx| i.set_value("", window, cx));
                let _ = p.update(cx, |p, cx| {
                    p.parent.unlocked = ok;
                    p.parent.said = (!ok).then(|| (false, "That isn't the password.".to_string()));
                    cx.notify();
                });
            })),
        );
        return body.children(said.map(|(ok, t)| div().text_sm().text_color(if ok { green } else { danger }).child(t))).into_any_element();
    }

    // On or off.
    body = body.child(
        div()
            .pt_2()
            .flex()
            .items_center()
            .gap_3()
            .child(Switch::new("parent-on").checked(config.enabled).on_click(|on: &bool, _, cx| {
                let on = *on;
                change(cx, move |s| s.parent.enabled = on);
            }))
            .child(div().font_weight(FontWeight::MEDIUM).text_color(fg).child(if config.enabled { "Parent mode is on" } else { "Parent mode is off" })),
    );
    if model.is_empty() {
        body = body.child(div().text_sm().text_color(danger).child("It needs a local model to check turns with: set one up in Settings > Summaries (LM Studio, Ollama). Nothing is sent anywhere else."));
    } else {
        body = body.child(note(&format!("Turns are checked by {model}, the local model from Summaries, on this machine. Only the result is kept (a flag and a time), never the text.")));
    }

    // The child.
    let name = field(&entity, "name", "First name", &config.name, false, window, cx);
    let n = name.clone();
    body = body.child(heading("Your child")).child(row("First name").child(div().flex_1().child(Input::new(&name).small())).child(Button::new("parent-name").small().outline().label("Save").on_click(move |_, _, cx| {
        let v = value(&n, cx);
        change(cx, move |s| s.parent.name = v);
    })));
    body = body.child(
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(Switch::new("parent-instructions").checked(config.instructions).on_click(|on: &bool, _, cx| {
                let on = *on;
                change(cx, move |s| s.parent.instructions = on);
            }))
            .child(div().flex_1().min_w_0().flex().flex_col().child(div().text_sm().text_color(fg).child("Tell the agent to stay a helpful tool")).child(note(
                "Warm and on the project; personal talk pointed back to you and friends; never asks for personal details. Claude Code sessions started after this get it (Cursor can't take instructions yet).",
            ))),
    );

    // Where alerts go.
    let topic = field(&entity, "topic", "a private topic name", &config.ntfy_topic, false, window, cx);
    let server = field(&entity, "server", "https://ntfy.sh", &config.ntfy_server, false, window, cx);
    let (t1, s1, t2, p_test) = (topic.clone(), server.clone(), topic.clone(), page.clone());
    body = body
        .child(heading("Alerts"))
        .child(note("Alerts go to your phone through ntfy, a free push service: install the ntfy app, subscribe to the topic below. Keep the topic private: anyone who knows it can read the alerts."))
        .child(
            row("Topic")
                .child(div().flex_1().child(Input::new(&topic).small()))
                .child(Button::new("parent-topic-new").small().ghost().label("Make one").on_click(move |_, window, cx| {
                    let t = keleuo_core::parent::new_topic();
                    t2.update(cx, |i, cx| i.set_value(t, window, cx));
                }))
                .child(Button::new("parent-topic").small().outline().label("Save").on_click(move |_, _, cx| {
                    let (t, s) = (value(&t1, cx), value(&s1, cx));
                    change(cx, move |c| {
                        c.parent.ntfy_topic = t;
                        c.parent.ntfy_server = if s.is_empty() { "https://ntfy.sh".into() } else { s };
                    });
                })),
        )
        .child(row("Server").child(div().flex_1().child(Input::new(&server).small())))
        .child(div().flex().child(Button::new("parent-test").small().outline().label("Send test alert").on_click(move |_, _, cx| {
            let config = cx.global::<crate::AppSettings>().0.borrow().parent.clone();
            let p = p_test.clone();
            say(&p, true, "Sending…", cx);
            cx.spawn(async move |cx: &mut AsyncApp| {
                let sent = cx
                    .background_spawn(async move { keleuo_core::parent::send(&config, "test", &format!("A test alert from Keleuo. Alerts about {} will look like this.", config.child()), false) })
                    .await;
                cx.update(|cx| match sent {
                    Ok(()) => say(&p, true, "Sent: it should be on your phone in a moment.", cx),
                    Err(e) => say(&p, false, format!("Not sent: {e:#}. Save the topic first."), cx),
                });
            })
            .detach();
        })));

    // When.
    let friend = field(&entity, "friend", "3", &config.friend_per_day.to_string(), false, window, cx);
    let chat = field(&entity, "chat", "5", &config.off_task_per_day.to_string(), false, window, cx);
    let (f1, c1, p_when) = (friend.clone(), chat.clone(), page.clone());
    body = body
        .child(heading("When to alert"))
        .child(note("Signs of distress and personal details shared: at once (at most once an hour each). The patterns: once they happen this many times in a day, then not again that day."))
        .child(row("Friend-talk, times a day").child(div().w(px(80.)).child(Input::new(&friend).small())))
        .child(row("Chat, not making, times a day").child(div().w(px(80.)).child(Input::new(&chat).small())))
        .child(div().flex().child(Button::new("parent-when").small().outline().label("Save").on_click(move |_, _, cx| {
            let (f, c) = (value(&f1, cx).parse::<u32>(), value(&c1, cx).parse::<u32>());
            match (f, c) {
                (Ok(f), Ok(c)) if f > 0 && c > 0 => {
                    change(cx, move |s| {
                        s.parent.friend_per_day = f;
                        s.parent.off_task_per_day = c;
                    });
                    say(&p_when, true, "Saved.", cx);
                }
                _ => say(&p_when, false, "Give whole numbers, 1 or more.", cx),
            }
        })));

    // The password.
    let pw = field(&entity, "password", if config.password.is_empty() { "Set a password" } else { "A new password" }, "", true, window, cx);
    let (pw1, p_pw, p_lock) = (pw.clone(), page.clone(), page.clone());
    let mut pass = row("Password").child(div().flex_1().child(Input::new(&pw).small())).child(Button::new("parent-password").small().outline().label("Set").on_click(move |_, window, cx| {
        let typed = pw1.read(cx).value().to_string();
        if typed.trim().is_empty() {
            return;
        }
        pw1.update(cx, |i, cx| i.set_value("", window, cx));
        change(cx, move |s| s.parent.set_password(&typed));
        say(&p_pw, true, "Password set: this page now needs it.", cx);
    }));
    if !config.password.is_empty() {
        let p_rm = page.clone();
        pass = pass
            .child(Button::new("parent-password-off").small().ghost().label("Remove").on_click(move |_, _, cx| {
                change(cx, |s| s.parent.set_password(""));
                say(&p_rm, true, "Password removed.", cx);
            }))
            .child(Button::new("parent-lock").small().ghost().label("Lock").on_click(move |_, _, cx| {
                let _ = p_lock.update(cx, |p, cx| {
                    p.parent.unlocked = false;
                    p.parent.said = None;
                    cx.notify();
                });
            }));
    }
    body = body
        .child(heading("Password"))
        .child(note("Locks this page, so turning parent mode off or changing it needs you. On a shared macOS account it's a speed bump: Keleuo's settings file can still be edited. Your child having their own macOS account is what really keeps it."))
        .child(pass);

    body.children(said.map(|(ok, t)| div().pt_1().text_sm().text_color(if ok { green } else { danger }).child(t)))
        .child(div().pt_3().border_t_1().border_color(border).text_xs().text_color(muted).child("The window shows a Parent mode badge while it's on, so your child knows."))
        .into_any_element()
}
