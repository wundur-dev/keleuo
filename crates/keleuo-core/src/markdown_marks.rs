//! Light Markdown marks for a message being written: where code, headings, bold and italic
//! text are, and which characters are Markdown's own (`**`, `#`, list markers) so they can be
//! shown quieter. The text itself is never changed; this only says how to colour it.

use std::ops::Range;

/// What a stretch of the message is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// Inline code or a fenced block.
    Code,
    /// A `#` heading's text.
    Heading,
    Bold,
    Italic,
    /// A `>` quote's text.
    Quote,
    /// Markdown's own characters: `**`, backticks, `#`, list markers, `>`.
    Syntax,
}

/// The marks in `text`, as UTF-8 byte ranges in order.
pub fn marks(text: &str) -> Vec<(Range<usize>, Mark)> {
    let mut out = Vec::new();
    let mut in_fence = false;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let start = offset;
        offset += line.len();
        let indent = body.len() - body.trim_start().len();
        let trimmed = body.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            out.push((start + indent..start + body.len(), Mark::Syntax));
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            if !body.is_empty() {
                out.push((start..start + body.len(), Mark::Code));
            }
            continue;
        }
        let mut rest_at = start + indent;
        let hashes = trimmed.len() - trimmed.trim_start_matches('#').len();
        if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            out.push((rest_at..rest_at + hashes, Mark::Syntax));
            let text_at = rest_at + hashes + 1;
            if text_at < start + body.len() {
                out.push((text_at..start + body.len(), Mark::Heading));
            }
            continue;
        }
        if let Some(q) = trimmed.strip_prefix("> ").or_else(|| (trimmed == ">").then_some("")) {
            out.push((rest_at..rest_at + 1, Mark::Syntax));
            let text_at = start + body.len() - q.len();
            if !q.is_empty() {
                out.push((text_at..start + body.len(), Mark::Quote));
            }
            continue;
        }
        if let Some(len) = list_marker(trimmed) {
            out.push((rest_at..rest_at + len, Mark::Syntax));
            rest_at += len;
        }
        inline(&text[rest_at..start + body.len()], rest_at, &mut out);
    }
    out
}

/// The length of a list marker at the start of `line` (`- `, `* `, `+ `, `12. `), if any.
fn list_marker(line: &str) -> Option<usize> {
    if line.starts_with("- ") || line.starts_with("* ") || line.starts_with("+ ") {
        return Some(1);
    }
    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    (digits > 0 && digits < 10 && line[digits..].starts_with(". ")).then_some(digits + 1)
}

/// Inline code, bold and italic in one line (`at` is where it starts in the message).
fn inline(line: &str, at: usize, out: &mut Vec<(Range<usize>, Mark)>) {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        // `code`
        if b[i] == b'`'
            && let Some(end) = line[i + 1..].find('`').map(|e| i + 1 + e)
            && end > i + 1
        {
            out.push((at + i..at + i + 1, Mark::Syntax));
            out.push((at + i + 1..at + end, Mark::Code));
            out.push((at + end..at + end + 1, Mark::Syntax));
            i = end + 1;
            continue;
        }
        // **bold** or __bold__
        if i + 1 < b.len() && (b[i] == b'*' || b[i] == b'_') && b[i + 1] == b[i] {
            let pair = &line[i..i + 2];
            if let Some(end) = line[i + 2..].find(pair).map(|e| i + 2 + e)
                && end > i + 2
                && !line[i + 2..end].starts_with(' ')
            {
                out.push((at + i..at + i + 2, Mark::Syntax));
                out.push((at + i + 2..at + end, Mark::Bold));
                out.push((at + end..at + end + 2, Mark::Syntax));
                i = end + 2;
                continue;
            }
        }
        // *italic*, or _italic_ starting a word (not snake_case)
        if (b[i] == b'*' || (b[i] == b'_' && (i == 0 || !b[i - 1].is_ascii_alphanumeric())))
            && i + 1 < b.len()
            && b[i + 1] != b' '
            && b[i + 1] != b[i]
            && let Some(end) = line[i + 1..].find(b[i] as char).map(|e| i + 1 + e)
            && b[end - 1] != b' '
            && (b[i] == b'*' || end + 1 >= b.len() || !b[end + 1].is_ascii_alphanumeric())
        {
            out.push((at + i..at + i + 1, Mark::Syntax));
            out.push((at + i + 1..at + end, Mark::Italic));
            out.push((at + end..at + end + 1, Mark::Syntax));
            i = end + 1;
            continue;
        }
        i += line[i..].chars().next().map_or(1, char::len_utf8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(text: &str) -> Vec<(&str, Mark)> {
        marks(text).into_iter().map(|(r, m)| (&text[r], m)).collect()
    }

    #[test]
    fn inline_code_bold_and_italic() {
        assert_eq!(
            named("use `cargo` and **always** *test*"),
            [
                ("`", Mark::Syntax),
                ("cargo", Mark::Code),
                ("`", Mark::Syntax),
                ("**", Mark::Syntax),
                ("always", Mark::Bold),
                ("**", Mark::Syntax),
                ("*", Mark::Syntax),
                ("test", Mark::Italic),
                ("*", Mark::Syntax),
            ]
        );
    }

    #[test]
    fn headings_lists_quotes_and_fences() {
        let text = "# Plan\n- one\n2. two\n> said\n```rust\nlet x = 1;\n```\nafter";
        assert_eq!(
            named(text),
            [
                ("#", Mark::Syntax),
                ("Plan", Mark::Heading),
                ("-", Mark::Syntax),
                ("2.", Mark::Syntax),
                (">", Mark::Syntax),
                ("said", Mark::Quote),
                ("```rust", Mark::Syntax),
                ("let x = 1;", Mark::Code),
                ("```", Mark::Syntax),
            ]
        );
    }

    #[test]
    fn plain_text_and_snake_case_are_left_alone() {
        assert_eq!(named("rename some_var_name to x * y"), []);
        assert_eq!(named("#hashtag and 3.5 apples"), []);
        assert_eq!(named("a lone ` backtick"), []);
    }
}
