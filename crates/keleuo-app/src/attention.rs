//! Telling the person a session needs them while they look elsewhere: a bounce of the Dock
//! icon, and the count of sessions waiting on it. [`Tracker`] decides when, and when to stay
//! quiet; [`bounce`] and [`set_badge`] say it (on macOS; elsewhere they do nothing). Banners
//! in Notification Center come once Keleuo runs as an app bundle.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// A second notice for the same session within this time is left out (a burst of prompts).
const QUIET: Duration = Duration::from_secs(8);

/// What a session waits on the person for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Need {
    /// A permission prompt is open: the tool and what it would do.
    Permission { tool: String, summary: String },
    /// A question in its terminal interface (a trust prompt, a menu).
    Asking,
    /// Its turn finished; the first line of its last reply, when there is one.
    Finished(Option<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Permission,
    Asking,
    Finished,
}

impl Need {
    fn kind(&self) -> Kind {
        match self {
            Need::Permission { .. } => Kind::Permission,
            Need::Asking => Kind::Asking,
            Need::Finished(_) => Kind::Finished,
        }
    }

    /// A permission prompt or a question (rather than a finished turn).
    pub fn is_question(&self) -> bool {
        self.kind() != Kind::Finished
    }

}


/// What to do after an update.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// Tell the person.
    Announce(Need),
    /// They looked: take back the notification left for this session.
    Withdraw,
}

/// What each session waits on, by session (an id that stays the same while it is open), so a
/// wait is announced once, not again while it lasts.
#[derive(Default)]
pub struct Tracker {
    waiting: HashMap<u64, Kind>,
    /// When each session was last announced.
    last: HashMap<u64, Instant>,
    /// Sessions with a notification left in Notification Center.
    posted: HashSet<u64>,
}

impl Tracker {
    /// The session `key` waits on `live` now (`Finished` only right after its turn ended);
    /// `looking` is true while it is shown in the focused window.
    pub fn update(&mut self, key: u64, live: Option<Need>, looking: bool, now: Instant) -> Action {
        let was = self.waiting.get(&key).copied();
        if looking {
            // Seen: a finished turn is read; a question still waits, but needs no notice.
            match live.as_ref().map(Need::kind) {
                Some(kind) if kind != Kind::Finished => self.waiting.insert(key, kind),
                _ => self.waiting.remove(&key),
            };
            return if self.posted.remove(&key) { Action::Withdraw } else { Action::Nothing };
        }
        let Some(need) = live else {
            // Nothing open now; a finished turn waits until it is seen.
            if was != Some(Kind::Finished) {
                self.waiting.remove(&key);
            }
            return Action::Nothing;
        };
        self.waiting.insert(key, need.kind());
        if was == Some(need.kind()) || self.last.get(&key).is_some_and(|t| now.duration_since(*t) < QUIET) {
            return Action::Nothing;
        }
        self.last.insert(key, now);
        self.posted.insert(key);
        Action::Announce(need)
    }

    /// The session closed.
    pub fn forget(&mut self, key: u64) {
        self.waiting.remove(&key);
        self.last.remove(&key);
        self.posted.remove(&key);
    }

    /// Sessions waiting: on a question (when `questions`), on a read of a finished turn (when
    /// `finished`).
    pub fn count(&self, questions: bool, finished: bool) -> usize {
        self.waiting
            .values()
            .filter(|k| if **k == Kind::Finished { finished } else { questions })
            .count()
    }
}

#[cfg(target_os = "macos")]
pub use mac::{bounce, set_badge};

#[cfg(not(target_os = "macos"))]
pub fn set_badge(_count: usize) {}
#[cfg(not(target_os = "macos"))]
pub fn bounce() {}

/// The Dock: its count and its bounce. (Banners in Notification Center need Keleuo to run as
/// an app bundle; they come with it.)
#[cfg(target_os = "macos")]
mod mac {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSRequestUserAttentionType};
    use objc2_foundation::NSString;

    /// The count on the Dock icon; none at zero.
    pub fn set_badge(count: usize) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let label = (count > 0).then(|| NSString::from_str(&count.to_string()));
        NSApplication::sharedApplication(mtm).dockTile().setBadgeLabel(label.as_deref());
    }

    /// Bounce the Dock icon once, when Keleuo is not the app in front.
    pub fn bounce() {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let app = NSApplication::sharedApplication(mtm);
        if !app.isActive() {
            app.requestUserAttention(NSRequestUserAttentionType::InformationalRequest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permission() -> Need {
        Need::Permission { tool: "Bash".into(), summary: "git push".into() }
    }

    #[test]
    fn a_wait_is_announced_once() {
        let mut t = Tracker::default();
        let now = Instant::now();
        assert_eq!(t.update(1, Some(permission()), false, now), Action::Announce(permission()));
        // Still waiting, later: no repeat.
        assert_eq!(t.update(1, Some(permission()), false, now + QUIET * 2), Action::Nothing);
        // Answered, then a new prompt after a while: announced again.
        assert_eq!(t.update(1, None, false, now + QUIET * 3), Action::Nothing);
        assert_eq!(t.update(1, Some(permission()), false, now + QUIET * 4), Action::Announce(permission()));
    }

    #[test]
    fn a_burst_is_one_notice() {
        let mut t = Tracker::default();
        let now = Instant::now();
        assert!(matches!(t.update(1, Some(permission()), false, now), Action::Announce(_)));
        t.update(1, None, false, now);
        assert_eq!(t.update(1, Some(Need::Asking), false, now + Duration::from_secs(1)), Action::Nothing);
        // Another session is its own event.
        assert!(matches!(t.update(2, Some(Need::Asking), false, now), Action::Announce(_)));
    }

    #[test]
    fn nothing_for_the_session_being_looked_at() {
        let mut t = Tracker::default();
        let now = Instant::now();
        assert_eq!(t.update(1, Some(permission()), true, now), Action::Nothing);
        // Looking away while it still waits: they have seen it already.
        assert_eq!(t.update(1, Some(permission()), false, now + QUIET * 2), Action::Nothing);
    }

    #[test]
    fn a_finished_turn_waits_until_seen_and_its_notice_is_taken_back() {
        let mut t = Tracker::default();
        let now = Instant::now();
        let done = Need::Finished(Some("All tests pass.".into()));
        assert_eq!(t.update(1, Some(done.clone()), false, now), Action::Announce(done));
        t.update(1, None, false, now);
        assert_eq!(t.count(true, true), 1);
        assert_eq!(t.count(true, false), 0);
        assert_eq!(t.update(1, None, true, now), Action::Withdraw);
        assert_eq!(t.count(true, true), 0);
        assert_eq!(t.update(1, None, true, now), Action::Nothing);
    }

    #[test]
    fn closing_a_session_forgets_it() {
        let mut t = Tracker::default();
        t.update(1, Some(Need::Asking), false, Instant::now());
        assert_eq!(t.count(true, true), 1);
        t.forget(1);
        assert_eq!(t.count(true, true), 0);
    }


}
