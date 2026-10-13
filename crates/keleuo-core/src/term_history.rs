//! What a terminal showed, kept across restarts: its scrollback and screen as text with colour
//! codes, one file per terminal in the app's cache, read back into a new terminal before its
//! shell starts. Clearing the terminal (`clear`) clears what is kept. Files older than 30 days
//! are removed.

use std::fmt::Write as _;
use std::path::PathBuf;

/// A colour as the terminal holds it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Ink {
    #[default]
    Default,
    /// One of the 16 named colours (0-7, bright 8-15).
    Named(u8),
    /// One of the 256.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// How a character is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Ink,
    pub bg: Ink,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

impl Style {
    /// The SGR sequence that sets this style from nothing.
    fn sgr(&self) -> String {
        let mut codes = vec!["0".to_string()];
        for (on, code) in [(self.bold, "1"), (self.dim, "2"), (self.italic, "3"), (self.underline, "4"), (self.inverse, "7")] {
            if on {
                codes.push(code.into());
            }
        }
        for (ink, base) in [(self.fg, 30u8), (self.bg, 40u8)] {
            match ink {
                Ink::Default => {}
                Ink::Named(n) if n < 8 => codes.push((base + n).to_string()),
                Ink::Named(n) => codes.push((base + 60 + (n - 8).min(7)).to_string()),
                Ink::Indexed(n) => codes.push(format!("{};5;{n}", base + 8)),
                Ink::Rgb(r, g, b) => codes.push(format!("{};2;{r};{g};{b}", base + 8)),
            }
        }
        format!("\x1b[{}m", codes.join(";"))
    }
}

/// Rows of cells, top to bottom, as text with colour codes. A row marked `wrapped` runs on
/// into the next (no line break, so the text reflows to the new width); trailing blanks and
/// blank rows at the end are left out.
pub fn to_ansi(rows: impl IntoIterator<Item = (Vec<(char, Style)>, bool)>) -> String {
    let mut out = String::new();
    let mut lines: Vec<(String, bool)> = Vec::new();
    for (cells, wrapped) in rows {
        let end = if wrapped { cells.len() } else { cells.iter().rposition(|(c, s)| *c != ' ' || s.bg != Ink::Default || s.inverse).map_or(0, |i| i + 1) };
        let mut line = String::new();
        let mut current = Style::default();
        for (c, style) in &cells[..end] {
            if *style != current {
                line.push_str(&style.sgr());
                current = *style;
            }
            line.push(*c);
        }
        if current != Style::default() {
            line.push_str("\x1b[0m");
        }
        lines.push((line, wrapped));
    }
    while lines.last().is_some_and(|(l, w)| l.is_empty() && !w) {
        lines.pop();
    }
    for (line, wrapped) in lines {
        out.push_str(&line);
        if !wrapped {
            out.push_str("\r\n");
        }
    }
    out
}

/// What goes before a new shell's first output: the kept text, then a quiet line saying so.
/// Nothing for a cleared terminal (at most its prompt was kept).
pub fn preface(kept: &str) -> Option<Vec<u8>> {
    let kept = kept.trim_start_matches(['\r', '\n']);
    if kept.lines().filter(|l| !l.trim().is_empty()).count() < 2 {
        return None;
    }
    let mut out = String::from(kept);
    if !out.ends_with("\r\n") {
        out.push_str("\r\n");
    }
    let _ = write!(out, "\x1b[0;2m── restored ──\x1b[0m\r\n");
    Some(out.into_bytes())
}

/// The folder of kept terminals in the cache (`KELEUO_CACHE_DIR` when set, for tests).
pub fn root() -> Option<PathBuf> {
    crate::images::root().and_then(|images| images.parent().map(|p| p.join("terminals")))
}

/// The file for terminal `id` (letters, digits and `-` only).
pub fn path(id: &str) -> Option<PathBuf> {
    let name: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    (!name.is_empty()).then(|| root().map(|r| r.join(format!("{name}.ansi")))).flatten()
}

/// A new terminal's id.
pub fn new_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{:x}-{:x}", nanos, std::process::id())
}

/// Remove kept terminals older than 30 days.
pub fn clean_old(now: std::time::SystemTime) {
    let Some(root) = root() else { return };
    let Ok(files) = std::fs::read_dir(&root) else { return };
    for f in files.flatten() {
        let old = f
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > crate::images::KEEP);
        if old {
            let _ = std::fs::remove_file(f.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str, style: Style) -> Vec<(char, Style)> {
        text.chars().map(|c| (c, style)).collect()
    }

    #[test]
    fn plain_rows_become_lines_without_trailing_blanks() {
        let rows = vec![(row("$ ls   ", Style::default()), false), (row("a  b   ", Style::default()), false), (row("       ", Style::default()), false)];
        assert_eq!(to_ansi(rows), "$ ls\r\na  b\r\n");
    }

    #[test]
    fn colours_are_kept_and_reset() {
        let green = Style { fg: Ink::Named(2), bold: true, ..Style::default() };
        let mut cells = row("ok", green);
        cells.extend(row(" done", Style::default()));
        assert_eq!(to_ansi(vec![(cells, false)]), "\x1b[0;1;32mok\x1b[0m done\r\n");
        let rgb = Style { fg: Ink::Rgb(1, 2, 3), bg: Ink::Indexed(200), ..Style::default() };
        assert_eq!(to_ansi(vec![(row("x", rgb), false)]), "\x1b[0;38;2;1;2;3;48;5;200mx\x1b[0m\r\n");
        let bright = Style { fg: Ink::Named(9), ..Style::default() };
        assert_eq!(to_ansi(vec![(row("e", bright), false)]), "\x1b[0;91me\x1b[0m\r\n");
    }

    #[test]
    fn a_wrapped_row_runs_on() {
        let rows = vec![(row("abc", Style::default()), true), (row("de ", Style::default()), false)];
        assert_eq!(to_ansi(rows), "abcde\r\n");
    }

    #[test]
    fn nothing_kept_gives_no_preface() {
        assert_eq!(preface("  \r\n"), None);
        assert_eq!(preface("\r\nme@mac proj %\r\n"), None, "a cleared terminal: its prompt alone");
        let p = String::from_utf8(preface("\r\n$ ls\r\na b\r\n").unwrap()).unwrap();
        assert!(p.starts_with("$ ls\r\na b\r\n") && p.contains("restored"), "{p}");
    }
}
