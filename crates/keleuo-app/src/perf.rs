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

/// What gets drawn, counted with `KELEUO_PERF=1` and reported by the pulse.
#[derive(Clone, Copy)]
pub enum Count {
    /// The window's root: one per frame drawn.
    Frame,
    Workspace,
    Chat,
    /// A terminal's grid painted.
    Terminal,
}

const COUNTS: [&str; 4] = ["frames", "workspace", "chat", "terminal"];

static COUNTED: [std::sync::atomic::AtomicU64; 4] = [const { std::sync::atomic::AtomicU64::new(0) }; 4];

pub fn count(what: Count) {
    if enabled() {
        COUNTED[what as usize].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// With `KELEUO_PERF=1`: every five seconds, on its own thread, what the app drew and what it
/// cost: renders per second, CPU, memory and wakeups (a line on stderr starting
/// `perf pulse`). A change is then measured against the same scene, not guessed.
pub fn start_pulse() {
    if !enabled() {
        return;
    }
    const EVERY: Duration = Duration::from_secs(5);
    std::thread::spawn(|| {
        let mut before = Usage::now();
        let mut counts_before = [0u64; 4];
        loop {
            std::thread::sleep(EVERY);
            let now = Usage::now();
            let secs = now.at.duration_since(before.at).as_secs_f64().max(0.001);
            let counts: Vec<u64> = COUNTED.iter().map(|c| c.load(std::sync::atomic::Ordering::Relaxed)).collect();
            let rates: Vec<String> = COUNTS
                .iter()
                .zip(counts.iter().zip(counts_before.iter()))
                .map(|(name, (n, b))| format!("{name} {:.1}/s", (n - b) as f64 / secs))
                .collect();
            eprintln!(
                "perf pulse {} · cpu {:.1}% · {:.0} MB · wakeups {:.0}/s",
                rates.join(" · "),
                (now.cpu - before.cpu).as_secs_f64() / secs * 100.0,
                now.footprint as f64 / 1_048_576.0,
                now.wakeups.saturating_sub(before.wakeups) as f64 / secs,
            );
            counts_before.copy_from_slice(&counts);
            before = now;
        }
    });
}

/// The process's CPU time, memory footprint and wakeups so far.
struct Usage {
    at: Instant,
    cpu: Duration,
    footprint: u64,
    wakeups: u64,
}

impl Usage {
    fn now() -> Self {
        let at = Instant::now();
        // SAFETY: both calls only write into the zeroed structs passed to them.
        let cpu = unsafe {
            let mut r: libc::rusage = std::mem::zeroed();
            libc::getrusage(libc::RUSAGE_SELF, &mut r);
            let t = |v: libc::timeval| Duration::new(v.tv_sec as u64, v.tv_usec as u32 * 1000);
            t(r.ru_utime) + t(r.ru_stime)
        };
        let (footprint, wakeups) = unsafe {
            let mut info: libc::rusage_info_v4 = std::mem::zeroed();
            let ok = libc::proc_pid_rusage(
                std::process::id() as libc::c_int,
                libc::RUSAGE_INFO_V4,
                &mut info as *mut _ as *mut libc::rusage_info_t,
            ) == 0;
            if ok { (info.ri_phys_footprint, info.ri_pkg_idle_wkups + info.ri_interrupt_wkups) } else { (0, 0) }
        };
        Self { at, cpu, footprint, wakeups }
    }
}
