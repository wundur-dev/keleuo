//! The reply Claude Code is writing, read off its terminal screen: the session file gets a
//! reply only once it is finished, while the screen shows it as it streams. The text here is a
//! preview (the screen already shows Markdown as terminal text, and lines are wrapped at its
//! width); the finished reply from the file replaces it. Anything that does not look as
//! expected gives `None`: no preview, nothing else changes.
//!
//! The layout read (Claude Code 2.1): the conversation, then a status line while it works
//! (`✻ Moseying… (4s · ↓ 141 tokens)`, `✻ Cooked for 7s · done`), sometimes a tip under it
//! (`  ⎿  Tip: …`), then the input box between two `────` rules. A text reply starts with
//! `⏺ ` and goes on indented by two spaces; a tool call shows as `⏺ Name(…)` or as a collapsed
//! line such as `  Listed 1 directory (ctrl+o to expand)`; the person's message starts `❯ `.

/// The reply being written, from the terminal's lines (scrollback first, then the screen) and
/// its width in columns.
pub fn reply_in_progress(lines: &[String], columns: usize) -> Option<String> {
    let rule = |l: &str| {
        let t = l.trim();
        t.chars().count() >= 20 && t.chars().all(|c| c == '─')
    };
    // The input box: the last `❯` line with a rule above it.
    let input = (1..lines.len()).rev().find(|&i| lines[i].starts_with('❯') && rule(&lines[i - 1]))?;
    let top_rule = input - 1;
    // Up from the box: blank lines, tips and the status line; a finished turn has nothing to
    // preview (the file has it, or will in a moment).
    let mut end = top_rule;
    while end > 0 {
        let l = lines[end - 1].trim_end();
        if l.is_empty() || l.trim_start().starts_with("⎿") {
            end -= 1;
        } else if is_status(l) {
            if l.contains("· done") || l.contains(" done ") {
                return None;
            }
            end -= 1;
        } else {
            break;
        }
    }
    // The last block above: a text reply (`⏺ ` and prose), not a tool or the person's message.
    let mut start = end;
    loop {
        if start == 0 {
            return None;
        }
        let l = &lines[start - 1];
        if l.starts_with('❯') {
            return None;
        }
        if let Some(rest) = l.strip_prefix("⏺ ") {
            if is_tool(rest) {
                return None;
            }
            start -= 1;
            break;
        }
        if is_tool(l.trim_start()) || l.trim_start().starts_with('⎿') {
            return None;
        }
        start -= 1;
    }
    let mut block: Vec<String> = Vec::new();
    for (k, l) in lines[start..end].iter().enumerate() {
        let l = l.trim_end();
        let text = if k == 0 { l.strip_prefix("⏺ ").unwrap_or(l) } else { l.strip_prefix("  ").unwrap_or(l) };
        block.push(text.to_string());
    }
    while block.last().is_some_and(|l| l.trim().is_empty()) {
        block.pop();
    }
    let text = unwrap(&block, columns);
    (!text.trim().is_empty()).then_some(text)
}

/// Whether `text`, typed as a message, still sits in the input box (its Enter was lost: the
/// interface was still busy starting, say). A long paste shows as `[Pasted text #1 …]`.
pub fn message_unsent(lines: &[String], text: &str) -> bool {
    let rule = |l: &str| {
        let t = l.trim();
        t.chars().count() >= 20 && t.chars().all(|c| c == '─')
    };
    let Some(input) = (1..lines.len()).rev().find(|&i| lines[i].starts_with('❯') && rule(&lines[i - 1])) else {
        return false;
    };
    let end = (input + 1..lines.len()).find(|&i| rule(&lines[i])).unwrap_or(lines.len());
    let typed: String = lines[input..end].join(" ");
    let typed = typed.trim_start_matches('❯');
    if typed.contains("[Pasted text") {
        return true;
    }
    let key: String = letters(text).chars().take(16).collect();
    !key.is_empty() && letters(typed).contains(&key)
}

fn letters(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// A line of Claude Code's own status while it works or when it is done: a spinner glyph at the
/// start, then words with `…` or a `·`.
fn is_status(line: &str) -> bool {
    let mut chars = line.chars();
    let Some(first) = chars.next() else { return false };
    !first.is_alphanumeric()
        && !matches!(first, ' ' | '⏺' | '❯' | '│' | '┌' | '├' | '└' | '-' | '*' | '•' | '`' | '#')
        && chars.next() == Some(' ')
        && (line.contains('…') || line.contains('·'))
}

/// A tool call as the screen shows it: `Name(…)` or a collapsed group (`… (ctrl+o to expand)`).
fn is_tool(text: &str) -> bool {
    if text.contains("(ctrl+o to expand)") || text.contains("(ctrl+r to expand)") {
        return true;
    }
    let name: String = text.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    !name.is_empty() && name.chars().next().is_some_and(char::is_uppercase) && text[name.len()..].starts_with('(')
}

/// Lines the terminal wrapped at its width, joined again (so the preview reflows to the chat's
/// width); lists, tables and short lines stay as they are.
fn unwrap(block: &[String], columns: usize) -> String {
    let near_edge = columns.saturating_sub(18).max(20);
    let structural = |l: &str| {
        let t = l.trim_start();
        t.starts_with("- ")
            || t.starts_with("* ")
            || t.starts_with("• ")
            || t.chars().next().is_some_and(|c| "│┌├└┐┤┘─".contains(c))
            || t.split_once(". ").is_some_and(|(n, _)| !n.is_empty() && n.len() <= 3 && n.chars().all(|c| c.is_ascii_digit()))
    };
    let mut out: Vec<String> = Vec::new();
    for line in block {
        let joinable = out.last().is_some_and(|prev: &String| {
            // The screen width the previous line took (with the two-space indent).
            prev.chars().count() + 2 >= near_edge && !prev.trim().is_empty() && !structural_box(prev)
        });
        if joinable && !line.trim().is_empty() && !structural(line) {
            let prev = out.last_mut().unwrap();
            prev.push(' ');
            prev.push_str(line.trim_start());
        } else {
            out.push(line.clone());
        }
    }
    out.join("\n")
}

fn structural_box(line: &str) -> bool {
    line.chars().any(|c| "│┌├└┐┤┘".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(name: &str) -> Vec<String> {
        let path = format!("{}/tests/fixtures/screens/{name}.txt", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(path).unwrap().lines().map(str::to_string).collect()
    }

    #[test]
    fn nothing_before_the_reply_starts() {
        assert_eq!(reply_in_progress(&screen("waiting"), 120), None);
    }

    #[test]
    fn a_reply_being_written_is_read_and_its_wrapped_lines_joined() {
        let text = reply_in_progress(&screen("streaming-paragraphs"), 120).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "How a Hash Map Works");
        assert!(lines[2].starts_with("A hash map stores key-value pairs") && lines[2].ends_with("constant time on average."), "{text}");
        assert!(lines[4].starts_with("Two different keys"), "{text}");
    }

    #[test]
    fn lists_stay_lists() {
        let text = reply_in_progress(&screen("streaming-lists"), 120).unwrap();
        assert!(text.contains("\n- Average O(1) insert, lookup, and removal\n- Worst case"), "{text}");
        assert!(text.contains("\n1. Hash the key into an integer.\n2."), "{text}");
    }

    #[test]
    fn a_reply_after_a_tool_is_read_without_the_tool() {
        let text = reply_in_progress(&screen("streaming-after-tool"), 120).unwrap();
        assert!(text.starts_with("The folder holds one file, README.md"), "{text}");
        assert!(!text.contains("Listed") && !text.contains("Moseying"), "{text}");
        let wrapped = reply_in_progress(&screen("streaming-wrapped"), 120).unwrap();
        assert_eq!(wrapped.lines().count(), 1, "one paragraph: {wrapped}");
        assert!(wrapped.ends_with("the count may be off."), "{wrapped}");
    }

    #[test]
    fn no_preview_while_a_tool_runs_or_once_done() {
        assert_eq!(reply_in_progress(&screen("tool-running"), 120), None);
        assert_eq!(reply_in_progress(&screen("finished-long"), 120), None);
        assert_eq!(reply_in_progress(&screen("finished-after-tool"), 120), None);
    }

    #[test]
    fn a_message_left_in_the_input_box_is_seen() {
        let mut lines = screen("waiting");
        let input = lines.iter().rposition(|l| l.starts_with('❯')).unwrap();
        assert!(!message_unsent(&lines, "Hello there, please look at the tests"), "the box is empty");
        lines[input] = "❯ Hello there, please look at the tests".into();
        assert!(message_unsent(&lines, "Hello there, please look at the tests"));
        lines[input] = "❯ [Pasted text #1 +40 lines]".into();
        assert!(message_unsent(&lines, "anything long"));
        // Sent: it shows above the box, which is empty again.
        assert!(!message_unsent(&screen("streaming-paragraphs"), "Without using any tools, explain in about 40 lines"));
    }

    #[test]
    fn an_unknown_screen_gives_nothing() {
        let lines: Vec<String> = ["$ ls", "a b c", ""].iter().map(|s| s.to_string()).collect();
        assert_eq!(reply_in_progress(&lines, 80), None);
        assert_eq!(reply_in_progress(&[], 80), None);
    }
}
