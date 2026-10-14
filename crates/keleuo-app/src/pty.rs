//! A shell in a pseudo-terminal: alacritty_terminal keeps the screen (grid, scrollback, modes)
//! and runs the PTY reader thread; the view only draws the grid and sends keys.

use std::borrow::Cow;
use std::sync::Arc;

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;
use keleuo_core::host::CommandSpec;

/// What the PTY thread tells the window.
#[derive(Debug, Clone)]
pub enum PtyEvent {
    /// New output: redraw.
    Wakeup,
    Title(String),
    /// The program wants text copied (OSC 52).
    Copy(String),
    Exited,
}

/// Forwards alacritty events to the window over a channel. The channel is unbounded but the
/// window drains it in one go, so a flood of output costs one redraw per frame, not per read.
#[derive(Clone)]
pub struct Listener {
    tx: async_channel::Sender<PtyEvent>,
    writer: Arc<FairMutex<Option<EventLoopSender>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        let ev = match event {
            TermEvent::Wakeup => PtyEvent::Wakeup,
            TermEvent::Title(t) => PtyEvent::Title(t),
            TermEvent::ClipboardStore(_, text) => PtyEvent::Copy(text),
            TermEvent::PtyWrite(text) => {
                if let Some(w) = self.writer.lock().as_ref() {
                    let _ = w.send(Msg::Input(Cow::Owned(text.into_bytes())));
                }
                return;
            }
            TermEvent::ChildExit(_) | TermEvent::Exit => PtyEvent::Exited,
            // A program asks for a colour (OSC 4/10/11/12), e.g. to tell a light background
            // from a dark one: answer with the palette in use.
            TermEvent::ColorRequest(index, format) => {
                let c = match index {
                    256 => crate::palette::foreground(),
                    257 => crate::palette::background(),
                    258 => crate::palette::cursor(),
                    i if i < 256 => crate::palette::indexed(i as u8),
                    _ => return,
                };
                let rgb = alacritty_terminal::vte::ansi::Rgb { r: (c >> 16) as u8, g: (c >> 8) as u8, b: c as u8 };
                if let Some(w) = self.writer.lock().as_ref() {
                    let _ = w.send(Msg::Input(Cow::Owned(format(rgb).into_bytes())));
                }
                return;
            }
            _ => return,
        };
        // Coalesce: a pending wakeup already covers this one.
        if matches!(ev, PtyEvent::Wakeup) && !self.tx.is_empty() {
            return;
        }
        let _ = self.tx.try_send(ev);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_w: u16,
    pub cell_h: u16,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

impl GridSize {
    fn window_size(self) -> WindowSize {
        WindowSize {
            num_lines: self.rows,
            num_cols: self.cols,
            cell_width: self.cell_w,
            cell_height: self.cell_h,
        }
    }
}

pub struct Pty {
    pub term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
    size: std::cell::Cell<GridSize>,
    #[cfg(unix)]
    master_fd: i32,
}

impl Pty {
    /// Start `spec` in a new terminal of `size`. `preface` is shown first, as if the program
    /// had written it (a kept terminal's earlier output).
    pub fn spawn(
        spec: &CommandSpec,
        size: GridSize,
        preface: Option<&[u8]>,
    ) -> anyhow::Result<(Self, async_channel::Receiver<PtyEvent>)> {
        let (tx, rx) = async_channel::unbounded();
        let writer = Arc::new(FairMutex::new(None));
        let listener = Listener {
            tx,
            writer: writer.clone(),
        };
        let config = Config {
            scrolling_history: 10_000,
            ..Config::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));
        if let Some(bytes) = preface {
            let mut parser: alacritty_terminal::vte::ansi::Processor = alacritty_terminal::vte::ansi::Processor::new();
            parser.advance(&mut *term.lock(), bytes);
        }
        let mut env = std::collections::HashMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
        env.insert("TERM_PROGRAM".to_string(), "Keleuo".to_string());
        // A terminal sets the locale; a window started from the Finder has none.
        if std::env::var_os("LANG").is_none() {
            env.insert("LANG".to_string(), "en_US.UTF-8".to_string());
        }
        env.extend(spec.env.iter().cloned());
        let options = tty::Options {
            shell: Some(tty::Shell::new(
                spec.program.to_string_lossy().to_string(),
                spec.args.clone(),
            )),
            working_directory: spec.cwd.clone(),
            drain_on_exit: true,
            env,
            #[cfg(windows)]
            escape_args: true,
        };
        let pty = tty::new(&options, size.window_size(), 0)?;
        #[cfg(unix)]
        let master_fd = {
            use std::os::fd::AsRawFd;
            pty.file().as_raw_fd()
        };
        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let sender = event_loop.channel();
        *writer.lock() = Some(sender.clone());
        event_loop.spawn();
        Ok((
            Self {
                term,
                sender,
                size: std::cell::Cell::new(size),
                #[cfg(unix)]
                master_fd,
            },
            rx,
        ))
    }

    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let _ = self.sender.send(Msg::Input(bytes.into()));
    }

    pub fn size(&self) -> GridSize {
        self.size.get()
    }

    /// Resize the grid and tell the program (SIGWINCH). Cheap when nothing changed.
    pub fn resize(&self, size: GridSize) {
        if size == self.size.get() || size.cols < 2 || size.rows < 1 {
            return;
        }
        self.size.set(size);
        let _ = self.sender.send(Msg::Resize(size.window_size()));
        self.term.lock().resize(size);
    }

    /// The name of the program in the foreground of the terminal (the shell when idle).
    pub fn foreground_program(&self) -> Option<String> {
        #[cfg(target_os = "macos")]
        unsafe {
            let pgid = libc::tcgetpgrp(self.master_fd);
            if pgid <= 0 {
                return None;
            }
            let mut buf = [0u8; 256];
            let n = libc::proc_name(pgid, buf.as_mut_ptr().cast(), buf.len() as u32);
            (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).to_string())
        }
        #[cfg(target_os = "linux")]
        unsafe {
            let pgid = libc::tcgetpgrp(self.master_fd);
            std::fs::read_to_string(format!("/proc/{pgid}/comm"))
                .ok()
                .map(|s| s.trim().to_string())
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        None
    }
}

impl Pty {
    /// End what runs in it, as closing a terminal window does: hang up on its foreground
    /// process group, ask it to quit, and stop reading. Other handles to the terminal may
    /// keep it on screen. Gives the process group hung up on, to make sure of its end (see
    /// [`gone`], [`kill`] and [`end_for_certain`]): a program may outlive the hang-up (Claude
    /// Code does), and once hung up its exit is no longer reported.
    pub fn end(&self) -> Option<i32> {
        #[cfg(unix)]
        let group = unsafe {
            let group = libc::tcgetpgrp(self.master_fd);
            if group > 0 {
                libc::killpg(group, libc::SIGHUP);
                libc::killpg(group, libc::SIGTERM);
            }
            (group > 0).then_some(group)
        };
        #[cfg(not(unix))]
        let group = None;
        let _ = self.sender.send(Msg::Shutdown);
        group
    }
}

/// Whether process group `group` has ended.
pub fn gone(group: i32) -> bool {
    #[cfg(unix)]
    return unsafe { libc::killpg(group, 0) } != 0;
    #[cfg(not(unix))]
    {
        let _ = group;
        true
    }
}

/// End process group `group` for certain.
pub fn kill(group: i32) {
    #[cfg(unix)]
    unsafe {
        libc::killpg(group, libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = group;
}

/// Hang up on what runs in `pty` and, if it has not ended a moment later, kill it (off the
/// UI thread; for an end nothing waits on).
pub fn end_for_certain(pty: &Pty) {
    if let Some(group) = pty.end() {
        std::thread::spawn(move || {
            for _ in 0..20 {
                if gone(group) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            kill(group);
        });
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}
