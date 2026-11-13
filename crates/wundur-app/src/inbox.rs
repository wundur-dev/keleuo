//! The inbox: notices from the agents of every session in one place. An agent sends one with
//! Wundur's `notify` tool (through `wundur-mcp`, like the messaging tools): that a long task is
//! done, that something waits on the person, that something went wrong. The toolbar's Inbox
//! button shows how many are unread and opens the list; a notice opens the session it came
//! from. A notice that asks something can carry choices; the person answers from the inbox
//! (a choice, or their own words) and the answer goes to the session as their next message.
//! Kept while the window is open (not saved).

use std::time::SystemTime;

use gpui_kit::*;

/// What a notice is about, for its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Info,
    Done,
    NeedsYou,
    Problem,
}

impl Kind {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().replace([' ', '-'], "_").as_str() {
            "done" | "finished" | "success" => Kind::Done,
            "needs_you" | "needs_input" | "question" | "waiting" => Kind::NeedsYou,
            "problem" | "error" | "failed" | "warning" => Kind::Problem,
            _ => Kind::Info,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Notice {
    pub id: u64,
    /// The sending session's number in the window (to open it).
    pub session: usize,
    /// Its name when it was sent.
    pub from: String,
    pub title: String,
    pub body: String,
    pub kind: Kind,
    /// Answers to offer, when it asks something.
    pub choices: Vec<String>,
    /// What the person answered from the inbox.
    pub answer: Option<String>,
    pub at: SystemTime,
    pub read: bool,
}

impl Notice {
    /// It asks something the person can answer from the inbox.
    pub fn asks(&self) -> bool {
        self.answer.is_none() && (self.kind == Kind::NeedsYou || !self.choices.is_empty())
    }
}

#[derive(Default)]
pub struct Inbox {
    next: u64,
    /// Newest first.
    pub items: Vec<Notice>,
}

impl Global for Inbox {}

/// At most this many notices are kept (the oldest go).
const KEEP: usize = 200;

#[allow(clippy::too_many_arguments)]
pub fn add(session: usize, from: String, title: String, body: String, kind: Kind, choices: Vec<String>, cx: &mut App) {
    let inbox = cx.default_global::<Inbox>();
    inbox.next += 1;
    let id = inbox.next;
    inbox.items.insert(0, Notice { id, session, from, title, body, kind, choices, answer: None, at: SystemTime::now(), read: false });
    inbox.items.truncate(KEEP);
    cx.refresh_windows();
}

pub fn items(cx: &App) -> Vec<Notice> {
    cx.try_global::<Inbox>().map(|i| i.items.clone()).unwrap_or_default()
}

pub fn unread(cx: &App) -> usize {
    cx.try_global::<Inbox>().map_or(0, |i| i.items.iter().filter(|n| !n.read).count())
}

pub fn mark_read(id: u64, cx: &mut App) {
    if let Some(n) = cx.default_global::<Inbox>().items.iter_mut().find(|n| n.id == id) {
        n.read = true;
    }
    cx.refresh_windows();
}

/// The notice `id`, if it is still there.
pub fn get(id: u64, cx: &App) -> Option<Notice> {
    cx.try_global::<Inbox>().and_then(|i| i.items.iter().find(|n| n.id == id).cloned())
}

/// The person answered notice `id` from the inbox.
pub fn answered(id: u64, answer: String, cx: &mut App) {
    if let Some(n) = cx.default_global::<Inbox>().items.iter_mut().find(|n| n.id == id) {
        n.answer = Some(answer);
        n.read = true;
    }
    cx.refresh_windows();
}

/// What the agent is sent when the person answers notice `n` with `answer`: the answer, said
/// to be about that notice (the agent may have sent several).
pub fn reply_text(n: &Notice, answer: &str) -> String {
    format!("(Answering your notice \"{}\" from Wundur's inbox.) {}", n.title, answer.trim())
}

pub fn mark_all_read(cx: &mut App) {
    for n in &mut cx.default_global::<Inbox>().items {
        n.read = true;
    }
    cx.refresh_windows();
}

pub fn clear(cx: &mut App) {
    cx.default_global::<Inbox>().items.clear();
    cx.refresh_windows();
}

#[cfg(test)]
mod tests {
    use super::Kind;

    #[test]
    fn kinds_are_read_loosely() {
        assert_eq!(Kind::parse("Needs you"), Kind::NeedsYou);
        assert_eq!(Kind::parse("needs-input"), Kind::NeedsYou);
        assert_eq!(Kind::parse("error"), Kind::Problem);
        assert_eq!(Kind::parse("done"), Kind::Done);
        assert_eq!(Kind::parse("whatever"), Kind::Info);
    }

    #[test]
    fn a_reply_names_the_notice_it_answers() {
        let n = super::Notice {
            id: 1,
            session: 1,
            from: "site".into(),
            title: "Pick a host".into(),
            body: String::new(),
            kind: Kind::NeedsYou,
            choices: vec!["VPS".into()],
            answer: None,
            at: std::time::SystemTime::now(),
            read: false,
        };
        assert!(n.asks());
        assert_eq!(super::reply_text(&n, " VPS "), "(Answering your notice \"Pick a host\" from Wundur's inbox.) VPS");
    }
}
