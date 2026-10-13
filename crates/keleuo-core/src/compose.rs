//! Writing a message. Shift+Enter on a list item (`1. `, `2) `, `- `, `* `, `• `) starts the
//! next item; on an item with nothing after its marker it ends the list instead.

/// What Shift+Enter does on a list line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListBreak {
    /// A new line starting with this marker (indent included), put in at the cursor.
    Continue(String),
    /// The line is only a marker: clear it, leaving an empty line.
    End,
}

/// What Shift+Enter does on `line` (the whole line, no newline) with the cursor at byte
/// `cursor` in it; `None` for a plain new line. In the middle of an item the line splits at
/// the cursor and the rest follows the new marker; inside the marker itself it is a plain
/// new line.
pub fn list_break(line: &str, cursor: usize) -> Option<ListBreak> {
    let (next, end) = marker(line)?;
    if line[end..].trim().is_empty() {
        return Some(ListBreak::End);
    }
    (cursor >= end).then_some(ListBreak::Continue(next))
}

/// The marker that follows the one `line` starts with, and where the item's text begins.
fn marker(line: &str) -> Option<(String, usize)> {
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let rest = &line[indent..];
    let (next, len) = if let Some(bullet) = ['-', '*', '•'].into_iter().find(|&b| rest.starts_with(b)) {
        (bullet.to_string(), bullet.len_utf8())
    } else {
        // At most nine digits, as in Markdown.
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 || digits > 9 {
            return None;
        }
        let close = rest[digits..].chars().next().filter(|&c| c == '.' || c == ')')?;
        let n: u64 = rest[..digits].parse().ok()?;
        (format!("{}{close}", n + 1), digits + 1)
    };
    let after = &rest[len..];
    let space = after.len() - after.trim_start_matches([' ', '\t']).len();
    if space == 0 {
        return None;
    }
    let next = format!("{}{next}{}", &line[..indent], &after[..space]);
    Some((next, indent + len + space))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cont(s: &str) -> Option<ListBreak> {
        Some(ListBreak::Continue(s.to_string()))
    }

    #[test]
    fn numbers_count_on() {
        assert_eq!(list_break("1. milk", 7), cont("2. "));
        assert_eq!(list_break("9) eggs", 7), cont("10) "));
        assert_eq!(list_break("  3. indented", 13), cont("  4. "));
    }

    #[test]
    fn bullets_repeat() {
        assert_eq!(list_break("- one", 5), cont("- "));
        assert_eq!(list_break("* one", 5), cont("* "));
        assert_eq!(list_break("\t• one", 7), cont("\t• "));
    }

    #[test]
    fn mid_item_splits_at_the_cursor() {
        assert_eq!(list_break("2. milk and eggs", 8), cont("3. "));
        // In the marker itself: a plain new line.
        assert_eq!(list_break("2. milk", 1), None);
    }

    #[test]
    fn a_bare_marker_ends_the_list() {
        assert_eq!(list_break("2. ", 3), Some(ListBreak::End));
        assert_eq!(list_break("  - ", 4), Some(ListBreak::End));
        assert_eq!(list_break("- ", 0), Some(ListBreak::End));
    }

    #[test]
    fn other_lines_are_plain() {
        assert_eq!(list_break("hello", 5), None);
        assert_eq!(list_break("", 0), None);
        assert_eq!(list_break("-not a bullet", 13), None);
        assert_eq!(list_break("**bold** text", 13), None);
        assert_eq!(list_break("2.5 litres", 10), None);
        assert_eq!(list_break("2.", 2), None);
        assert_eq!(list_break("1234567890. too long", 20), None);
    }
}
