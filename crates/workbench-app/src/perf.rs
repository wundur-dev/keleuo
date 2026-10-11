//! Opt-in timing (`WORKBENCH_PERF=1`): paint times of the terminal and the chat, and the
//! time from a key press in the terminal to the frame that shows the echoed glyph. A summary
//! goes to stderr every two seconds while there is activity. Off, it costs one branch.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("WORKBENCH_PERF").is_ok_and(|v| v != "0"))
}

#[derive(Default)]
struct Samples {
    name: &'static str,
    values: Vec<f64>,
}

struct State {
    sets: Vec<Samples>,
    last_report: Instant,
    /// When the last terminal key was pressed and its output not yet painted.
    pending_key: Option<Instant>,
    /// Output arrived after that key.
    key_output: bool,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        sets: Vec::new(),
        last_report: Instant::now(),
        pending_key: None,
        key_output: false,
    });
}

pub fn record(name: &'static str, d: Duration) {
    if !enabled() {
        return;
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        match s.sets.iter_mut().find(|x| x.name == name) {
            Some(set) => set.values.push(d.as_secs_f64() * 1000.0),
            None => s.sets.push(Samples {
                name,
                values: vec![d.as_secs_f64() * 1000.0],
            }),
        }
        if s.last_report.elapsed() > Duration::from_secs(2) {
            report(&mut s);
        }
    });
}

pub fn key_pressed() {
    if enabled() {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.pending_key = Some(Instant::now());
            s.key_output = false;
        });
    }
}

/// The PTY produced output (the echo of the key, usually).
pub fn pty_output() {
    if enabled() {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            if s.pending_key.is_some() {
                s.key_output = true;
            }
        });
    }
}

/// A terminal frame was painted: if it carries output that followed a key, that is the
/// key-to-glyph time.
pub fn terminal_painted() {
    if !enabled() {
        return;
    }
    let done = STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.key_output {
            s.key_output = false;
            s.pending_key.take()
        } else {
            None
        }
    });
    if let Some(start) = done {
        record("key to glyph (incl. shell echo)", start.elapsed());
    }
}

fn report(s: &mut State) {
    for set in &mut s.sets {
        if set.values.is_empty() {
            continue;
        }
        set.values.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = set.values.len();
        let p = |q: f64| set.values[((n as f64 - 1.0) * q).round() as usize];
        eprintln!(
            "perf {:<34} n={:<5} p50={:>6.2}ms p95={:>6.2}ms max={:>6.2}ms",
            set.name,
            n,
            p(0.5),
            p(0.95),
            set.values[n - 1]
        );
        set.values.clear();
    }
    s.last_report = Instant::now();
}
