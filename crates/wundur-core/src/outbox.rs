//! Messages the person sent while the agent was busy. Each waits its turn, in the order sent,
//! and goes when the turn before it has ended: one message per turn, so the chat shows them
//! where the agent got them. "Send now" sends one at once: into the running turn when the
//! agent takes messages while it works, else after stopping that turn.

use std::collections::VecDeque;
use std::path::PathBuf;

/// A message waiting to go.
#[derive(Clone, Debug, PartialEq)]
pub struct Waiting {
    /// Stays the same while it waits (its place in the list may change).
    pub id: u64,
    /// As written (paste markers and all), sent as if it were sent now.
    pub text: String,
    pub images: Vec<PathBuf>,
}

impl Waiting {
    /// Its first line with text, cut to `max` characters (an ellipsis when cut, or when more
    /// lines follow).
    pub fn preview(&self, max: usize) -> String {
        let text = crate::paste::without_markers(&self.text);
        let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
        let first = lines.next().unwrap_or_default();
        let more = lines.next().is_some();
        let mut out: String = first.chars().take(max).collect();
        if first.chars().count() > max || more {
            out = out.trim_end().to_string();
            out.push('…');
        }
        out
    }
}

/// What "Send now" does with a waiting message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendNow {
    /// Send it now (the agent is idle, or takes messages while it works).
    Now,
    /// Stop the turn; the message goes first when it has stopped.
    StopFirst,
}

impl SendNow {
    pub fn decide(busy: bool, takes_mid_turn: bool) -> Self {
        if busy && !takes_mid_turn { SendNow::StopFirst } else { SendNow::Now }
    }
}

/// The messages waiting, in the order they go.
#[derive(Clone, Debug, Default)]
pub struct Outbox {
    items: VecDeque<Waiting>,
    next_id: u64,
}

impl Outbox {
    /// Add a message at the end; its id.
    pub fn push(&mut self, text: impl Into<String>, images: Vec<PathBuf>) -> u64 {
        self.next_id += 1;
        self.items.push_back(Waiting { id: self.next_id, text: text.into(), images });
        self.next_id
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn items(&self) -> impl Iterator<Item = &Waiting> {
        self.items.iter()
    }

    /// Take message `id` out (sent now, or dropped).
    pub fn remove(&mut self, id: u64) -> Option<Waiting> {
        let i = self.items.iter().position(|w| w.id == id)?;
        self.items.remove(i)
    }

    /// Put message `id` first, to go next. False when there is no such message.
    pub fn to_front(&mut self, id: u64) -> bool {
        match self.remove(id) {
            Some(w) => {
                self.items.push_front(w);
                true
            }
            None => false,
        }
    }

    /// The message to send now: the first, once the agent is free (`busy`: a turn runs).
    pub fn next(&mut self, busy: bool) -> Option<Waiting> {
        if busy { None } else { self.items.pop_front() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(o: &Outbox) -> Vec<&str> {
        o.items().map(|w| w.text.as_str()).collect()
    }

    #[test]
    fn messages_go_in_order_one_per_free_turn() {
        let mut o = Outbox::default();
        o.push("one", Vec::new());
        o.push("two", vec![PathBuf::from("/tmp/a.png")]);
        o.push("three", Vec::new());
        // Nothing goes while a turn runs.
        assert_eq!(o.next(true), None);
        assert_eq!(o.len(), 3);
        assert_eq!(o.next(false).map(|w| w.text), Some("one".into()));
        let two = o.next(false).unwrap();
        assert_eq!((two.text.as_str(), two.images.len()), ("two", 1));
        assert_eq!(texts(&o), ["three"]);
    }

    #[test]
    fn one_can_go_first_or_be_dropped() {
        let mut o = Outbox::default();
        let a = o.push("a", Vec::new());
        let b = o.push("b", Vec::new());
        let c = o.push("c", Vec::new());
        assert!(o.to_front(c));
        assert_eq!(texts(&o), ["c", "a", "b"]);
        assert_eq!(o.remove(a).map(|w| w.text), Some("a".into()));
        assert_eq!(texts(&o), ["c", "b"]);
        // Gone already.
        assert_eq!(o.remove(a), None);
        assert!(!o.to_front(a));
        // Ids are not reused.
        let d = o.push("d", Vec::new());
        assert!(d > c && d != b);
    }

    #[test]
    fn send_now_stops_the_turn_only_for_an_agent_that_cannot_take_it() {
        assert_eq!(SendNow::decide(true, true), SendNow::Now);
        assert_eq!(SendNow::decide(true, false), SendNow::StopFirst);
        assert_eq!(SendNow::decide(false, false), SendNow::Now);
    }

    #[test]
    fn the_preview_is_the_first_line() {
        let w = |text: &str| Waiting { id: 1, text: text.into(), images: Vec::new() };
        assert_eq!(w("fix the tests").preview(40), "fix the tests");
        assert_eq!(w("\n  first line  \nsecond").preview(40), "first line…");
        assert_eq!(w("a long line of text").preview(6), "a long…");
        assert_eq!(w("").preview(10), "");
    }
}
