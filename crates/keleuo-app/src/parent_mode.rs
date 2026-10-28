//! Parent mode in the window ([`keleuo_core::parent`]): the agent's extra instructions, and the
//! check after each turn, which alerts the parent when a pattern appears. Nothing the child
//! wrote is kept or sent to the parent.

use gpui_kit::*;
use keleuo_core::parent::{self, Log};

/// The turns already checked (their text), so none is checked twice.
#[derive(Default)]
struct Checked(std::collections::HashSet<String>);
impl Global for Checked {}

/// The instructions an agent starts with, with parent mode on.
pub fn instructions(cx: &App) -> Option<String> {
    let s = cx.global::<crate::AppSettings>().0.borrow();
    (s.parent.enabled && s.parent.instructions).then(|| parent::instructions(&s.parent))
}

/// Whether parent mode is on (the window's badge).
pub fn on(cx: &App) -> bool {
    cx.global::<crate::AppSettings>().0.borrow().parent.enabled
}

/// Check the turn that just ended in `chat` (from `from`), off the UI thread: ask the local
/// model, note the flags, send the alerts due.
pub fn check_turn(chat: &keleuo_core::conversation::Conversation, from: usize, cx: &mut App) {
    let (config, summaries) = {
        let s = cx.global::<crate::AppSettings>().0.borrow();
        (s.parent.clone(), s.summaries.clone())
    };
    if !config.enabled || summaries.model.trim().is_empty() {
        return;
    }
    let Some((child, agent)) = parent::turn_text(chat, from) else { return };
    // A turn ends once however it is seen (the chat and the terminal both say so).
    let key = format!("{}\0{}", child, agent);
    if !cx.default_global::<Checked>().0.insert(key) {
        return;
    }
    cx.background_spawn(async move {
        let flags = match parent::check(&summaries, &child, &agent) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("keleuo: parent mode: {e:#}");
                return;
            }
        };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let mut log = Log::load();
        let due = log.note(flags, now, &config);
        log.save();
        for alert in due {
            if let Err(e) = parent::send_alert(&config, &alert, &clock(alert.since)) {
                eprintln!("keleuo: parent mode: alert not sent: {e:#}");
            }
        }
    })
    .detach();
}

/// A time today, as "4:12 pm".
fn clock(secs: u64) -> String {
    use chrono::TimeZone as _;
    chrono::Local.timestamp_opt(secs as i64, 0).single().map(|t| t.format("%-I:%M %P").to_string()).unwrap_or_default()
}
