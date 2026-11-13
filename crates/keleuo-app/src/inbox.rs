//! The inbox: notices from the agents of every session in one place. An agent sends one with
//! Keleuo's `notify` tool (through `keleuo-mcp`, like the messaging tools): that a long task is
//! done, that something waits on the person, that something went wrong. The toolbar's Inbox
//! button shows how many are unread and opens the list; a notice opens the session it came
//! from. Kept while the window is open (not saved).

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
    pub at: SystemTime,
    pub read: bool,
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

pub fn add(session: usize, from: String, title: String, body: String, kind: Kind, cx: &mut App) {
    let inbox = cx.default_global::<Inbox>();
    inbox.next += 1;
    let id = inbox.next;
    inbox.items.insert(0, Notice { id, session, from, title, body, kind, at: SystemTime::now(), read: false });
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
}
