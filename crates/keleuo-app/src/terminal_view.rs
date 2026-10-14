//! The terminal panel: one or more shells, each drawn from its alacritty grid.
//!
//! Drawing: per visible row, cell backgrounds as quads, then the row's text shaped once with a
//! run per style change. Nothing redraws unless the PTY thread reports new output (coalesced
//! to one wakeup per frame) or the user does something.

use std::cell::Cell;
use std::rc::Rc;

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::CursorShape;
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::palette;
use crate::pty::{GridSize, Pty, PtyEvent};

/// The fixed-width font every system has: Menlo on macOS, Consolas on Windows.
pub const FONT_FAMILY: &str = if cfg!(windows) { "Consolas" } else if cfg!(target_os = "linux") { "DejaVu Sans Mono" } else { "Menlo" };

/// The placeholder over a starting program goes once its output has paused this long, or this
/// long after its first output at most, looked for this often, and fades out over this.
const SETTLE_QUIET: std::time::Duration = std::time::Duration::from_millis(250);
const SETTLE_AT_MOST: std::time::Duration = std::time::Duration::from_secs(3);
const SETTLE_CHECK: std::time::Duration = std::time::Duration::from_millis(80);
const PLACEHOLDER_FADE: std::time::Duration = std::time::Duration::from_millis(180);
/// The font size, from Settings.
pub fn font_size(cx: &App) -> f32 {
    crate::prefs(cx).terminal_font_size()
}

fn hsla(rgb: u32) -> Hsla {
    gpui_kit::rgb(rgb).into()
}

/// What one turn of the wheel does in a terminal.
#[derive(Debug, PartialEq, Eq)]
pub enum Wheel {
    /// Scroll our own scrollback by this many lines (up is positive).
    Scrollback(i32),
    /// Send this to the program.
    Send(String),
}

/// The wheel, as terminals do it: a program that asked for mouse reports gets wheel events
/// (buttons 64 and 65; SGR form when it asked for that); a full-screen program in the
/// alternate screen with alternate scroll on gets arrow keys; otherwise the scrollback
/// scrolls. `col` and `row` are 1-based cells under the pointer.
pub fn wheel_input(mode: TermMode, lines: i32, col: usize, row: usize) -> Wheel {
    let n = lines.unsigned_abs() as usize;
    if mode.intersects(TermMode::MOUSE_MODE) {
        let button = if lines > 0 { 64 } else { 65 };
        let one = if mode.contains(TermMode::SGR_MOUSE) {
            format!("\x1b[<{button};{col};{row}M")
        } else {
            // The old X10 form: each value plus 32, as one character (cells past 223 clamp).
            let c = |v: usize| char::from((32 + v.min(223)) as u8);
            format!("\x1b[M{}{}{}", c(button), c(col), c(row))
        };
        return Wheel::Send(one.repeat(n));
    }
    if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
        let key = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
            (true, true) => "\x1bOA",
            (false, true) => "\x1bOB",
            (true, false) => "\x1b[A",
            (false, false) => "\x1b[B",
        };
        return Wheel::Send(key.repeat(n));
    }
    Wheel::Scrollback(lines)
}

pub struct TerminalView {
    pub pty: Option<Rc<Pty>>,
    pub focus: FocusHandle,
    pub title: Option<String>,
    pub exited: bool,
    /// Output arrived since the user last looked (the sidebar shows a dot).
    pub unseen: bool,
    /// The program has drawn something (the screen is no longer blank).
    pub drawn: bool,
    /// Shown over the screen while the program starts: the shape of the conversation's last
    /// lines, a spinner and what is starting. It stays until the program has drawn and then
    /// gone quiet for a moment (an interface draws its banner, then the conversation, in
    /// steps), then fades out.
    pub placeholder: Option<Placeholder>,
    /// The program's first screen is complete: the placeholder fades (from `revealed`).
    settled: bool,
    revealed: Option<std::time::Instant>,
    /// When output last came, and when the first did.
    last_output: Option<std::time::Instant>,
    first_output: Option<std::time::Instant>,
    settling: Option<Task<()>>,
    /// For an agent's interface: what Cmd+V with an image does (see [`ImageSupport`]).
    pub images: Option<ImageSupport>,
    /// The image whose `[Image #N]` marker is under the pointer, and where.
    hover_image: Option<(std::path::PathBuf, Point<Pixels>)>,
    error: Option<String>,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    cell: Rc<Cell<Size<Pixels>>>,
    selecting: bool,
    /// Wheel movement smaller than a line, kept for the next event (trackpads send many
    /// small deltas).
    wheel_rest: f32,
    /// 1, 2, ... in opening order: the key of its saved name.
    pub number: usize,
    /// For the automatic name: the folder, its git branch and the login shell.
    pub folder: std::path::PathBuf,
    pub branch: Option<String>,
    _events: Option<Task<()>>,
    /// Kept across restarts under this id (shell terminals; not an agent's own interface).
    pub history: Option<String>,
    /// Writing what it shows, a moment after its output stops.
    save: Option<Task<()>>,
}

/// Keeps a pasted image (its bytes and file extension).
pub type PastedFn = dyn Fn(Vec<u8>, &'static str, &mut App);
/// The image behind `[Image #N]`.
pub type ImageFn = dyn Fn(usize, &App) -> Option<std::path::PathBuf>;

/// Images in an agent's interface: Cmd+V with an image on the clipboard sends `paste_key` (the
/// interface reads the clipboard itself) after `pasted` keeps a copy; `image` gives the image
/// behind an `[Image #N]` marker on screen, for a preview while the pointer is over it.
#[derive(Clone)]
pub struct ImageSupport {
    pub paste_key: Vec<u8>,
    pub pasted: Rc<PastedFn>,
    pub image: Rc<ImageFn>,
}

/// The `N` of an `[Image #N]` marker in `line` covering column `col`.
pub fn image_marker_at(line: &str, col: usize) -> Option<usize> {
    let chars: Vec<char> = line.chars().collect();
    let text: String = chars.iter().collect();
    let mut from = 0;
    while let Some(i) = text[from..].find("[Image #") {
        let start = from + i;
        let end = start + text[start..].find(']')?;
        let (s, e) = (text[..start].chars().count(), text[..end].chars().count());
        if (s..=e).contains(&col) {
            return text[start + 8..end].parse().ok();
        }
        from = end;
    }
    None
}

/// What a terminal shows while its program starts.
pub struct Placeholder {
    /// The last lines of the conversation so far, oldest first.
    pub lines: Vec<(bool, String)>,
    /// "Starting Claude Code..."
    pub label: String,
}

pub enum TerminalEvent {
    Closed,
    Activated,
    /// Cmd+click: the text of the row clicked and the column (in characters), for whoever
    /// knows the project's files to open the file named there.
    OpenLink { line: String, column: usize },
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl TerminalView {
    pub fn new(spec: keleuo_core::host::CommandSpec, cx: &mut Context<Self>) -> Self {
        Self::start(spec, None, cx)
    }

    /// A shell terminal kept across restarts as `id`: what it showed last time comes first.
    pub fn kept(spec: keleuo_core::host::CommandSpec, id: String, cx: &mut Context<Self>) -> Self {
        Self::start(spec, Some(id), cx)
    }

    fn start(spec: keleuo_core::host::CommandSpec, history: Option<String>, cx: &mut Context<Self>) -> Self {
        // Settings: not kept, so neither shown again nor saved.
        let history = history.filter(|_| crate::prefs(cx).keep_terminal_history);
        let preface = history
            .as_deref()
            .and_then(keleuo_core::term_history::path)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|kept| keleuo_core::term_history::preface(&kept));
        let focus = cx.focus_handle();
        // Until it is first drawn (an agent's interface may start in a hidden view).
        let initial = GridSize {
            cols: 120,
            rows: 36,
            cell_w: 8,
            cell_h: 16,
        };
        let (pty, error, events) = match Pty::spawn(&spec, initial, preface.as_deref()) {
            Ok((pty, rx)) => {
                let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    while let Ok(first) = rx.recv().await {
                        // Take everything already queued: one redraw for the lot.
                        let mut batch = vec![first];
                        while let Ok(more) = rx.try_recv() {
                            batch.push(more);
                        }
                        let alive = this.update(cx, |view, cx| {
                            for ev in batch {
                                match ev {
                                    PtyEvent::Wakeup => {
                                        crate::perf::pty_output();
                                        view.unseen = true;
                                        view.schedule_save(cx);
                                        if !view.drawn && !view.screen_text().trim().is_empty() {
                                            view.drawn = true;
                                            crate::perf::switch_step("terminal drew its first screen");
                                        }
                                        view.output_came(cx);
                                    }
                                    PtyEvent::Title(t) => view.title = Some(t),
                                    PtyEvent::Copy(text) => {
                                        cx.write_to_clipboard(ClipboardItem::new_string(text))
                                    }
                                    PtyEvent::Exited => {
                                        view.exited = true;
                                        crate::perf::switch_step("terminal process exited");
                                    }
                                }
                            }
                            cx.notify();
                        });
                        if alive.is_err() {
                            break;
                        }
                    }
                });
                (Some(Rc::new(pty)), None, Some(task))
            }
            Err(e) => (None, Some(format!("Could not start the shell: {e}")), None),
        };
        Self {
            pty,
            focus,
            title: None,
            exited: false,
            unseen: false,
            drawn: false,
            placeholder: None,
            settled: false,
            revealed: None,
            last_output: None,
            first_output: None,
            settling: None,
            images: None,
            hover_image: None,
            error,
            bounds: Rc::new(Cell::new(Bounds::default())),
            cell: Rc::new(Cell::new(size(px(8.), px(16.)))),
            selecting: false,
            wheel_rest: 0.,
            number: 0,
            folder: std::path::PathBuf::new(),
            branch: None,
            _events: events,
            history,
            save: None,
        }
    }

    /// Keep what it shows, two seconds after its output stops (a running program writing
    /// steadily is saved at most every two seconds).
    /// Output came: once the screen has something and the output pauses (or after a while,
    /// for a program that never pauses), the placeholder fades.
    fn output_came(&mut self, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        self.last_output = Some(now);
        self.first_output.get_or_insert(now);
        if self.settled || self.placeholder.is_none() || !self.drawn || self.settling.is_some() {
            return;
        }
        self.settling = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                cx.background_executor().timer(SETTLE_CHECK).await;
                let done = this.update(cx, |view, cx| {
                    let quiet = view.last_output.is_none_or(|t| t.elapsed() >= SETTLE_QUIET);
                    let long = view.first_output.is_some_and(|t| t.elapsed() >= SETTLE_AT_MOST);
                    if !(quiet || long || view.exited) {
                        return false;
                    }
                    view.settled = true;
                    view.revealed = Some(std::time::Instant::now());
                    view.settling = None;
                    cx.notify();
                    true
                });
                if done.unwrap_or(true) {
                    break;
                }
            }
            // Once more when the fade is over, to take the placeholder away.
            cx.background_executor().timer(PLACEHOLDER_FADE).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        }));
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        if self.history.is_none() || self.save.is_some() || !crate::prefs(cx).keep_terminal_history {
            return;
        }
        self.save = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
            let Ok(Some((path, text))) = this.update(cx, |view, _| {
                view.save = None;
                let path = view.history.as_deref().and_then(keleuo_core::term_history::path)?;
                Some((path, view.kept_text()?))
            }) else {
                return;
            };
            // Detached: this task's handle was just let go, which would cancel an await here.
            cx.background_spawn(async move {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(path, text);
            })
            .detach();
        }));
    }

    /// The scrollback (its last 3,000 lines) and screen as text with colour codes; `None`
    /// while a full-screen program (an editor, a pager) has the screen, so what was kept
    /// before stays.
    fn kept_text(&self) -> Option<String> {
        use keleuo_core::term_history::{Ink, Style, to_ansi};
        use alacritty_terminal::vte::ansi::Color;
        let pty = self.pty.as_ref()?;
        let term = pty.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let grid = term.grid();
        let ink = |c: Color| match c {
            Color::Named(n) if (n as usize) < 16 => Ink::Named(n as u8),
            Color::Named(_) => Ink::Default,
            Color::Indexed(n) => Ink::Indexed(n),
            Color::Spec(rgb) => Ink::Rgb(rgb.r, rgb.g, rgb.b),
        };
        let cols = grid.columns();
        let first = -(grid.history_size().min(3000) as i32);
        let rows = (first..grid.screen_lines() as i32).map(|l| {
            let row = &grid[Line(l)];
            let cells = (0..cols)
                .map(|c| &row[Column(c)])
                .filter(|cell| !cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER))
                .map(|cell| {
                    let f = cell.flags;
                    let style = Style {
                        fg: ink(cell.fg),
                        bg: ink(cell.bg),
                        bold: f.contains(Flags::BOLD),
                        dim: f.contains(Flags::DIM),
                        italic: f.contains(Flags::ITALIC),
                        underline: f.intersects(Flags::ALL_UNDERLINES),
                        inverse: f.contains(Flags::INVERSE),
                    };
                    // A tab is kept as the space it takes, so columns line up when replayed.
                    (if cell.c == '\t' { ' ' } else { cell.c }, style)
                })
                .collect::<Vec<_>>();
            (cells, row[Column(cols - 1)].flags.contains(Flags::WRAPLINE))
        });
        Some(to_ansi(rows.collect::<Vec<_>>()))
    }

    fn mode(&self) -> TermMode {
        self.pty
            .as_ref()
            .map(|p| *p.term.lock().mode())
            .unwrap_or(TermMode::empty())
    }

    /// The program asked for bracketed paste.
    pub fn bracketed_paste(&self) -> bool {
        self.mode().contains(TermMode::BRACKETED_PASTE)
    }

    fn write_str(&self, s: &str) {
        if let Some(pty) = &self.pty {
            pty.write(s.as_bytes().to_vec());
        }
    }

    fn scroll_to_bottom(&self) {
        if let Some(pty) = &self.pty {
            pty.term.lock().scroll_display(Scroll::Bottom);
        }
    }

    /// The visible screen as text, one line per row (tests read it over the control channel).
    /// The last `max` lines of scrollback and the screen, top to bottom (trailing spaces
    /// trimmed), with the width in columns.
    pub fn recent_lines(&self, max: usize) -> Option<(Vec<String>, usize)> {
        let pty = self.pty.as_ref()?;
        let term = pty.term.lock();
        let grid = term.grid();
        let screen = grid.screen_lines() as i32;
        let history = (grid.history_size() as i32).min(max.saturating_sub(screen as usize) as i32);
        let lines = (-history..screen)
            .map(|l| {
                let row = &grid[Line(l)];
                let text: String = (0..grid.columns()).map(|c| row[Column(c)].c).collect();
                text.trim_end().to_string()
            })
            .collect();
        Some((lines, grid.columns()))
    }

    pub fn screen_text(&self) -> String {
        let Some(pty) = &self.pty else { return String::new() };
        let term = pty.term.lock();
        let grid = term.grid();
        let mut out = String::new();
        for line in 0..grid.screen_lines() {
            let row = &grid[Line(line as i32)];
            let text: String = (0..grid.columns()).map(|c| row[Column(c)].c).collect();
            out.push_str(text.trim_end());
            out.push('\n');
        }
        out
    }

    fn copy(&self, cx: &mut Context<Self>) -> bool {
        let Some(pty) = &self.pty else { return false };
        let text = pty.term.lock().selection_to_string();
        match text {
            Some(t) if !t.is_empty() => {
                cx.write_to_clipboard(ClipboardItem::new_string(t));
                true
            }
            _ => false,
        }
    }

    /// Tests: Cmd+V.
    pub fn test_paste(&mut self, cx: &mut Context<Self>) {
        self.paste(cx);
    }

    fn paste(&self, cx: &mut Context<Self>) {
        let item = cx.read_from_clipboard();
        if let (Some(images), Some(item)) = (self.images.clone(), item.as_ref()) {
            let image = item.entries().iter().find_map(|e| match e {
                ClipboardEntry::Image(i) => Some(i.clone()),
                _ => None,
            });
            if let Some(image) = image {
                // The interface pastes it itself (its own `[Image #N]`); keep a copy to show.
                let ext = match image.format {
                    ImageFormat::Jpeg => "jpg",
                    ImageFormat::Gif => "gif",
                    ImageFormat::Webp => "webp",
                    _ => "png",
                };
                (images.pasted)(image.bytes.clone(), ext, cx);
                if let Some(pty) = &self.pty {
                    pty.write(images.paste_key.clone());
                }
                return;
            }
        }
        let Some(text) = item.and_then(|c| c.text()) else {
            return;
        };
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.mode().contains(TermMode::BRACKETED_PASTE) {
            self.write_str(&format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', "")));
        } else {
            self.write_str(&text);
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let m = ks.modifiers;
        if m.platform {
            match ks.key.as_str() {
                "c" => {
                    if self.copy(cx) {
                        cx.stop_propagation();
                    }
                }
                "v" => {
                    self.paste(cx);
                    cx.stop_propagation();
                }
                "k" => {
                    // Clear, as in Terminal.app: the scrollback goes too (and so does what is
                    // kept of it), then the shell redraws its prompt at the top.
                    if let Some(pty) = &self.pty {
                        // Screen and scrollback cleared here first: cleared by the shell alone,
                        // the screen would be pushed into the scrollback.
                        let mut parser: alacritty_terminal::vte::ansi::Processor = alacritty_terminal::vte::ansi::Processor::new();
                        parser.advance(&mut *pty.term.lock(), b"\x1b[H\x1b[2J\x1b[3J");
                    }
                    self.write_str("\x0c");
                    self.schedule_save(cx);
                    cx.stop_propagation();
                }
                _ => {}
            }
            return;
        }
        if m.shift && matches!(ks.key.as_str(), "pageup" | "pagedown") {
            if let Some(pty) = &self.pty {
                let rows = pty.size().rows as i32;
                let delta = if ks.key == "pageup" { rows } else { -rows };
                pty.term.lock().scroll_display(Scroll::Delta(delta));
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
        if let Some(bytes) = key_bytes(ks, self.mode()) {
            self.scroll_to_bottom();
            if let Some(pty) = &self.pty {
                pty.term.lock().selection = None;
                crate::perf::key_pressed();
                pty.write(bytes);
            }
            cx.stop_propagation();
        }
    }

    fn grid_point(&self, position: Point<Pixels>) -> (GridPoint, Side) {
        let b = self.bounds.get();
        let c = self.cell.get();
        let x = (position.x - b.origin.x).max(px(0.));
        let y = (position.y - b.origin.y).max(px(0.));
        let col = (x / c.width) as usize;
        let row = (y / c.height) as usize;
        let side = if (x / c.width).fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        let Some(pty) = &self.pty else {
            return (GridPoint::new(Line(0), Column(0)), side);
        };
        let term = pty.term.lock();
        let offset = term.grid().display_offset() as i32;
        let row = row.min(term.screen_lines().saturating_sub(1));
        let col = col.min(term.columns().saturating_sub(1));
        (GridPoint::new(Line(row as i32 - offset), Column(col)), side)
    }

    /// The text of a row of the grid (scrollback included), one character per cell.
    fn row_text(&self, line: Line) -> String {
        let Some(pty) = &self.pty else { return String::new() };
        let term = pty.term.lock();
        let grid = term.grid();
        if line.0 >= -(grid.history_size() as i32) && line.0 < grid.screen_lines() as i32 {
            let row = &grid[line];
            (0..grid.columns()).map(|c| row[Column(c)].c).collect()
        } else {
            String::new()
        }
    }

    fn mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let (point, side) = self.grid_point(e.position);
        if e.modifiers.platform && e.click_count == 1 {
            let line = self.row_text(point.line);
            cx.emit(TerminalEvent::OpenLink { line, column: point.column.0 });
            return;
        }
        if let Some(pty) = &self.pty {
            let ty = match e.click_count {
                2 => SelectionType::Semantic,
                3 => SelectionType::Lines,
                _ => SelectionType::Simple,
            };
            let mut term = pty.term.lock();
            term.selection = (e.click_count > 1).then(|| Selection::new(ty, point, side));
            if e.click_count == 1 {
                term.selection = Some(Selection::new(SelectionType::Simple, point, side));
            }
        }
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(images) = self.images.clone()
            && self.pty.is_some()
        {
            // An `[Image #N]` under the pointer: show that image.
            let (point, _) = self.grid_point(e.position);
            let line = self.row_text(point.line);
            let found = image_marker_at(&line, point.column.0).and_then(|n| (images.image)(n, cx)).map(|p| (p, e.position));
            if found.as_ref().map(|f| &f.0) != self.hover_image.as_ref().map(|h| &h.0) {
                self.hover_image = found;
                cx.notify();
            }
        }
        if !self.selecting || e.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let (point, side) = self.grid_point(e.position);
        if let Some(pty) = &self.pty
            && let Some(sel) = pty.term.lock().selection.as_mut()
        {
            sel.update(point, side);
        }
        cx.notify();
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        if let Some(pty) = &self.pty {
            let mut term = pty.term.lock();
            if term.selection.as_ref().is_some_and(|s| s.is_empty()) {
                term.selection = None;
            }
        }
        cx.notify();
    }

    fn scroll(&mut self, e: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(pty) = &self.pty else { return };
        let line_h = self.cell.get().height;
        let exact = self.wheel_rest + e.delta.pixel_delta(line_h).y / line_h;
        let lines = exact.trunc() as i32;
        self.wheel_rest = exact.fract();
        if lines == 0 {
            return;
        }
        let (cell, _) = self.grid_point(e.position);
        let col = cell.column.0 + 1;
        let row = (cell.line.0 + pty.term.lock().grid().display_offset() as i32).max(0) as usize + 1;
        match wheel_input(self.mode(), lines, col, row) {
            Wheel::Scrollback(n) => pty.term.lock().scroll_display(Scroll::Delta(n)),
            Wheel::Send(bytes) => self.write_str(&bytes),
        }
        cx.notify();
    }
}

/// The bytes a key sends to the program.
fn key_bytes(ks: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let m = ks.modifiers;
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let csi = |code: &str, letter: char| -> Vec<u8> {
        let modifier = 1 + (m.shift as u8) + (m.alt as u8) * 2 + (m.control as u8) * 4;
        if modifier > 1 {
            format!("\x1b[1;{modifier}{letter}").into_bytes()
        } else if app_cursor && code.is_empty() {
            format!("\x1bO{letter}").into_bytes()
        } else {
            format!("\x1b[{code}{letter}").into_bytes()
        }
    };
    let tilde = |n: u8| format!("\x1b[{n}~").into_bytes();
    let bytes = match ks.key.as_str() {
        "enter" => {
            if m.alt {
                b"\x1b\r".to_vec()
            } else {
                b"\r".to_vec()
            }
        }
        "backspace" => {
            if m.alt || m.control {
                b"\x1b\x7f".to_vec()
            } else {
                b"\x7f".to_vec()
            }
        }
        "tab" => {
            if m.shift {
                b"\x1b[Z".to_vec()
            } else {
                b"\t".to_vec()
            }
        }
        "escape" => b"\x1b".to_vec(),
        "up" => csi("", 'A'),
        "down" => csi("", 'B'),
        "right" => csi("", 'C'),
        "left" => csi("", 'D'),
        "home" => csi("", 'H'),
        "end" => csi("", 'F'),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "delete" => tilde(3),
        "insert" => tilde(2),
        "f1" => b"\x1bOP".to_vec(),
        "f2" => b"\x1bOQ".to_vec(),
        "f3" => b"\x1bOR".to_vec(),
        "f4" => b"\x1bOS".to_vec(),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        key => {
            if m.control {
                let c = key.chars().next()?;
                if key.chars().count() != 1 {
                    return None;
                }
                let code = match c {
                    'a'..='z' => c as u8 - b'a' + 1,
                    '@' | ' ' | '2' => 0,
                    '[' | '3' => 27,
                    '\\' | '4' => 28,
                    ']' | '5' => 29,
                    '^' | '6' => 30,
                    '_' | '-' | '7' => 31,
                    _ => return None,
                };
                let mut v = Vec::new();
                if m.alt {
                    v.push(0x1b);
                }
                v.push(code);
                v
            } else {
                // Option on macOS composes characters (key_char); Alt elsewhere is Meta.
                let text = ks
                    .key_char
                    .clone()
                    .or_else(|| (key.chars().count() == 1).then(|| key.to_string()))?;
                let mut v = Vec::new();
                if m.alt && !cfg!(target_os = "macos") {
                    v.push(0x1b);
                }
                v.extend(text.into_bytes());
                v
            }
        }
    };
    Some(bytes)
}

impl TerminalView {
    /// The program in the foreground when it is not the shell, else the folder and branch.
    pub fn auto_name(&self) -> String {
        let program = self.pty.as_ref().and_then(|p| p.foreground_program());
        let shell = std::env::var("SHELL").unwrap_or_default();
        let mut name = keleuo_core::tabs::auto_name(
            &self.folder,
            self.branch.as_deref(),
            program.as_deref(),
            &shell,
        );
        if self.number > 1 && program.as_deref().is_none_or(|p| p.contains("sh")) {
            name = format!("{name} {}", self.number);
        }
        if self.exited {
            name.push_str(" (exited)");
        }
        name
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let pty = self.pty.clone();
        let pty_paint = self.pty.clone();
        let bounds_cell = self.bounds.clone();
        let cell_cell = self.cell.clone();
        let cell_paint = self.cell.clone();
        let base = div()
            .id("terminal")
            .size_full()
            .bg(hsla(palette::background()))
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .cursor(CursorStyle::IBeam);
        if let Some(err) = &self.error {
            return base.p_2().text_color(hsla(if palette::is_dark() { 0xcc6666 } else { 0xc0392b })).child(err.clone());
        }
        let text_size = font_size(cx);
        // Fading out once settled, then gone.
        let fading = self.revealed.is_some_and(|t| t.elapsed() < PLACEHOLDER_FADE);
        let placeholder = self
            .placeholder
            .as_ref()
            .filter(|_| (!self.settled && !self.exited) || fading)
            .map(|p| {
                // The conversation's last lines as bars of their length (a skeleton: its shape,
                // not its text, which the program is about to draw its own way).
                let cell = self.cell.get().width;
                let bar = hsla(palette::dim()).opacity(0.22);
                // From the top, where the interface draws: its banner (a logo and two lines),
                // then the conversation.
                let line = |w: f32| div().h(px(text_size * 0.75)).w(cell * w).rounded(px(3.)).bg(bar);
                let mut col = div()
                    .absolute()
                    .inset_0()
                    .px_2()
                    .pt_3()
                    .bg(hsla(palette::background()))
                    .flex()
                    .flex_col()
                    .gap(px(text_size * 0.7))
                    .overflow_hidden()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(cell * 2.)
                            .pb(px(text_size))
                            .child(div().w(cell * 8.).h(px(text_size * 3.)).rounded(px(4.)).bg(bar))
                            .child(div().flex().flex_col().gap(px(text_size * 0.6)).child(line(18.)).child(line(30.))),
                    );
                for (user, line) in &p.lines {
                    let chars = line.chars().count().clamp(4, 120) as f32;
                    col = col.child(
                        div()
                            .h(px(text_size * 0.75))
                            .when(*user, |d| d.ml(cell * 2.))
                            .w(cell * chars)
                            .max_w(relative(0.92))
                            .rounded(px(3.))
                            .bg(if *user { bar.opacity(0.32) } else { bar }),
                    );
                }
                col.child(
                    div()
                        .pt_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(text_size))
                        .text_color(hsla(palette::dim()))
                        .child(gpui_kit::component::spinner::Spinner::new().color(hsla(palette::dim())))
                        .child(p.label.clone()),
                )
            })
            .map(|col| match self.revealed {
                // Animated only while fading: it starts when the fade does.
                Some(_) => col.with_animation("placeholder-fade", Animation::new(PLACEHOLDER_FADE), |d, t| d.opacity(1. - t)).into_any_element(),
                None => col.into_any_element(),
            });
        base.px_1().relative().child(
            canvas(
                move |bounds, window, cx| {
                    bounds_cell.set(bounds);
                    let font_size = px(font_size(cx));
                    let font = font(FONT_FAMILY);
                    let ts = window.text_system();
                    let font_id = ts.resolve_font(&font);
                    let cw = ts
                        .advance(font_id, font_size, 'm')
                        .map(|s| s.width)
                        .unwrap_or(px(7.5));
                    let ch = (font_size * 1.35).round();
                    cell_cell.set(size(cw, ch));
                    if let Some(pty) = &pty {
                        let cols = ((bounds.size.width / cw).floor() as u16).max(2);
                        let rows = ((bounds.size.height / ch).floor() as u16).max(1);
                        pty.resize(GridSize {
                            cols,
                            rows,
                            cell_w: f32::from(cw) as u16,
                            cell_h: f32::from(ch) as u16,
                        });
                    }
                },
                move |bounds, (), window, cx| {
                    let Some(pty) = &pty_paint else { return };
                    let start = std::time::Instant::now();
                    paint_grid(pty, bounds, cell_paint.get(), focused, window, cx);
                    crate::perf::record("terminal paint", start.elapsed());
                    crate::perf::terminal_painted();
                },
            )
            .size_full(),
        )
        .children(placeholder)
        .children(self.hover_image.clone().map(|(path, at)| {
            let b = self.bounds.get();
            let (x, y) = (at.x - b.origin.x + px(12.), at.y - b.origin.y + px(16.));
            div()
                .absolute()
                .left(x)
                .top(y)
                .p_1()
                .rounded_md()
                .bg(hsla(palette::background()))
                .border_1()
                .border_color(hsla(palette::dim()))
                .shadow_lg()
                .child(img(path).max_w(px(320.)).max_h(px(240.)).object_fit(ObjectFit::Contain))
        }))
    }
}

fn paint_grid(
    pty: &Pty,
    bounds: Bounds<Pixels>,
    cell: Size<Pixels>,
    focused: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let term = pty.term.lock();
    let content = term.renderable_content();
    let colors = content.colors;
    let offset = content.display_offset as i32;
    let selection = content.selection;
    let cols = term.columns();
    let rows = term.screen_lines();
    let font_size = px(font_size(cx));
    let base_font = font(FONT_FAMILY);
    let default_bg = palette::background();
    let grid = term.grid();

    for row in 0..rows {
        let line = Line(row as i32 - offset);
        let y = bounds.origin.y + cell.height * row as f32;
        let mut text = String::with_capacity(cols);
        let mut runs: Vec<TextRun> = Vec::new();
        let mut bg_start: Option<(usize, u32)> = None;
        let paint_bg = |from: usize, to: usize, color: u32, window: &mut Window| {
            if color != default_bg {
                window.paint_quad(fill(
                    Bounds::new(
                        point(bounds.origin.x + cell.width * from as f32, y),
                        size(cell.width * (to - from) as f32, cell.height),
                    ),
                    hsla(color),
                ));
            }
        };
        for col in 0..cols {
            let gp = GridPoint::new(line, Column(col));
            let c = &grid[gp];
            let mut fg_color = if c.flags.contains(Flags::BOLD) {
                palette::brighten(c.fg)
            } else {
                c.fg
            };
            let mut bg_color = c.bg;
            if c.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg_color, &mut bg_color);
            }
            let mut fg = palette::resolve(fg_color, colors, true);
            let mut bg = palette::resolve(bg_color, colors, false);
            if selection.is_some_and(|s| s.contains(gp)) {
                bg = palette::selection();
                fg = palette::foreground();
            }
            match bg_start {
                Some((start, color)) if color != bg => {
                    paint_bg(start, col, color, window);
                    bg_start = Some((col, bg));
                }
                None => bg_start = Some((col, bg)),
                _ => {}
            }
            if c.flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let ch = if c.c == '\t' || c.c == '\0' || c.flags.contains(Flags::HIDDEN) {
                ' '
            } else {
                c.c
            };
            let before = text.len();
            text.push(ch);
            if let Some(zw) = c.zerowidth() {
                text.extend(zw.iter());
            }
            let len = text.len() - before;
            let mut f = base_font.clone();
            if c.flags.contains(Flags::BOLD) {
                f.weight = FontWeight::BOLD;
            }
            if c.flags.contains(Flags::ITALIC) {
                f.style = FontStyle::Italic;
            }
            let mut color = hsla(fg);
            if c.flags.contains(Flags::DIM) {
                color.a = 0.7;
            }
            let underline = c
                .flags
                .intersects(Flags::ALL_UNDERLINES)
                .then(|| UnderlineStyle {
                    thickness: px(1.),
                    color: Some(color),
                    wavy: c.flags.contains(Flags::UNDERCURL),
                });
            let strike = c
                .flags
                .contains(Flags::STRIKEOUT)
                .then(|| StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(color),
                });
            match runs.last_mut() {
                Some(r)
                    if r.font == f
                        && r.color == color
                        && r.underline == underline
                        && r.strikethrough == strike =>
                {
                    r.len += len
                }
                _ => runs.push(TextRun {
                    len,
                    font: f,
                    color,
                    background_color: None,
                    underline,
                    strikethrough: strike,
                }),
            }
        }
        if let Some((start, color)) = bg_start {
            paint_bg(start, cols, color, window);
        }
        let trimmed = text.trim_end().len();
        if trimmed == 0 {
            continue;
        }
        // Drop trailing blanks from the runs too.
        let mut left = trimmed;
        let mut kept = Vec::new();
        for mut r in runs {
            if left == 0 {
                break;
            }
            r.len = r.len.min(left);
            left -= r.len;
            kept.push(r);
        }
        text.truncate(trimmed);
        let shaped =
            window
                .text_system()
                .shape_line(text.into(), font_size, &kept, Some(cell.width));
        let _ = shaped.paint(
            point(bounds.origin.x, y),
            cell.height,
            TextAlign::Left,
            None,
            window,
            cx,
        );
    }

    // Cursor.
    let cursor = content.cursor;
    if cursor.shape != CursorShape::Hidden {
        let row = cursor.point.line.0 + offset;
        if row >= 0 && (row as usize) < rows {
            let origin = point(
                bounds.origin.x + cell.width * cursor.point.column.0 as f32,
                bounds.origin.y + cell.height * row as f32,
            );
            let color = hsla(palette::cursor());
            let block = Bounds::new(origin, cell);
            if !focused {
                window.paint_quad(outline(block, color, BorderStyle::Solid));
            } else {
                match cursor.shape {
                    CursorShape::Beam => window
                        .paint_quad(fill(Bounds::new(origin, size(px(2.), cell.height)), color)),
                    CursorShape::Underline => window.paint_quad(fill(
                        Bounds::new(
                            point(origin.x, origin.y + cell.height - px(2.)),
                            size(cell.width, px(2.)),
                        ),
                        color,
                    )),
                    _ => {
                        let mut c = color;
                        c.a = 0.6;
                        window.paint_quad(fill(block, c));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn finds_the_image_marker_under_the_pointer() {
        let line = "❯ [Image #1] [Image #12] what is this";
        assert_eq!(image_marker_at(line, 4), Some(1));
        assert_eq!(image_marker_at(line, 15), Some(12));
        assert_eq!(image_marker_at(line, 30), None);
    }

    #[::core::prelude::v1::test]
    fn the_wheel_scrolls_the_normal_screen() {
        // A shell at its prompt: the default modes include alternate scroll, but there is no
        // alternate screen, so nothing reaches the shell (no history cycling).
        assert_eq!(wheel_input(TermMode::default(), 3, 5, 5), Wheel::Scrollback(3));
        assert_eq!(wheel_input(TermMode::default(), -2, 5, 5), Wheel::Scrollback(-2));
    }

    #[::core::prelude::v1::test]
    fn full_screen_programs_get_arrows() {
        let mode = TermMode::default() | TermMode::ALT_SCREEN;
        assert_eq!(wheel_input(mode, 2, 1, 1), Wheel::Send("\x1b[A\x1b[A".into()));
        assert_eq!(wheel_input(mode | TermMode::APP_CURSOR, -1, 1, 1), Wheel::Send("\x1bOB".into()));
        // Alternate scroll turned off: the scrollback.
        assert_eq!(wheel_input(TermMode::ALT_SCREEN, 1, 1, 1), Wheel::Scrollback(1));
    }

    #[::core::prelude::v1::test]
    fn programs_that_want_the_mouse_get_wheel_events() {
        // Claude Code's interface: alternate screen, mouse reports, SGR form.
        let mode = TermMode::default() | TermMode::ALT_SCREEN | TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(wheel_input(mode, 1, 10, 4), Wheel::Send("\x1b[<64;10;4M".into()));
        assert_eq!(wheel_input(mode, -2, 10, 4), Wheel::Send("\x1b[<65;10;4M".repeat(2)));
        let x10 = TermMode::MOUSE_DRAG;
        assert_eq!(wheel_input(x10, 1, 1, 1), Wheel::Send("\x1b[M`!!".into()));
    }
}
