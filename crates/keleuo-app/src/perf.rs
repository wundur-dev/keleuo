//! Opt-in timing (`KELEUO_PERF=1`): paint times of the terminal and the chat, and the
//! time from a key press in the terminal to the frame that shows the echoed glyph. A summary
//! goes to stderr every two seconds while there is activity. Off, it costs one branch.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| keleuo_core::env_var("PERF").is_ok_and(|v| v != "0"))
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

/// With `KELEUO_PERF=1`: how long a startup step took (stderr), to find what holds up the
/// first frame.
pub fn step<T>(name: &str, f: impl FnOnce() -> T) -> T {
    if !enabled() {
        return f();
    }
    let t = Instant::now();
    let out = f();
    eprintln!("keleuo: step {name}: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    out
}

/// The first frame of the first window, measured from process start; printed once.
pub fn first_frame(window: &mut gpui_kit::Window) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    window.on_next_frame(|_, _| {
        if let Some(start) = crate::START.get() {
            eprintln!("keleuo: first frame {:.0} ms after start", start.elapsed().as_secs_f64() * 1000.0);
        }
    });
}

thread_local! {
    /// When the current Chat or Terminal switch began.
    static SWITCH: RefCell<Option<Instant>> = const { RefCell::new(None) };
}

/// With `KELEUO_PERF=1`: a Chat or Terminal switch starts now (the click).
pub fn switch_begin(what: &str) {
    if enabled() {
        SWITCH.with(|s| *s.borrow_mut() = Some(Instant::now()));
        eprintln!("keleuo: switch {what}: begin");
    }
}

/// A step of the current switch, with the time since its click.
pub fn switch_step(what: &str) {
    if !enabled() {
        return;
    }
    SWITCH.with(|s| {
        if let Some(start) = *s.borrow() {
            eprintln!("keleuo: switch   {what}: +{:.0} ms", start.elapsed().as_secs_f64() * 1000.0);
        }
    });
}

/// The view after a switch is being drawn: report when its frame is on screen, once.
pub fn switch_shown(window: &mut gpui_kit::Window) {
    if !enabled() {
        return;
    }
    let pending = SWITCH.with(|s| s.borrow_mut().take());
    if let Some(start) = pending {
        window.on_next_frame(move |_, _| {
            eprintln!("keleuo: switch   shown: +{:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
        });
    }
}
