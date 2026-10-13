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
    /// A link's text (`[text](url)`), or a bare web address.
    Link,
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

/// Whether `text` has a code fence (a line starting with ``` or ~~~).
pub fn has_fence(text: &str) -> bool {
    text.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with("```") || t.starts_with("~~~")
    })
}

/// The fenced code blocks of `text` that name a language (```` ```rust ````): the language
/// and the byte range of the code between the fences (to the end of the text while the
/// closing fence is not typed yet).
pub fn fences(text: &str) -> Vec<(String, Range<usize>)> {
    let mut out = Vec::new();
    let mut open: Option<(String, usize)> = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("```").or_else(|| trimmed.strip_prefix("~~~")) else { continue };
        match open.take() {
            Some((lang, from)) => out.push((lang, from..start)),
            None => open = Some((rest.split_whitespace().next().unwrap_or("").to_lowercase(), offset)),
        }
    }
    if let Some((lang, from)) = open {
        out.push((lang, from..text.len()));
    }
    out.retain(|(lang, range)| !lang.is_empty() && !range.is_empty());
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
        // [text](url): the text as a link, the brackets and address quieter
        if b[i] == b'['
            && let Some(close) = line[i + 1..].find("](").map(|e| i + 1 + e)
            && close > i + 1
            && let Some(end) = line[close + 2..].find(')').map(|e| close + 2 + e)
            && !line[close + 2..end].contains(char::is_whitespace)
        {
            out.push((at + i..at + i + 1, Mark::Syntax));
            out.push((at + i + 1..at + close, Mark::Link));
            out.push((at + close..at + end + 1, Mark::Syntax));
            i = end + 1;
            continue;
        }
        // A bare address, starting a word; trailing punctuation is the sentence's.
        if (i == 0 || !b[i - 1].is_ascii_alphanumeric())
            && (line[i..].starts_with("https://") || line[i..].starts_with("http://"))
        {
            let len = line[i..].find(|c: char| c.is_whitespace() || c == '>').unwrap_or(line.len() - i);
            let url = line[i..i + len].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '"', '\'']);
            out.push((at + i..at + i + url.len(), Mark::Link));
            i += url.len().max(1);
            continue;
        }
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
    fn links_and_bare_addresses() {
        assert_eq!(
            named("see [the docs](https://x.dev/a_b) and https://example.com/p?q=1."),
            [
                ("[", Mark::Syntax),
                ("the docs", Mark::Link),
                ("](https://x.dev/a_b)", Mark::Syntax),
                ("https://example.com/p?q=1", Mark::Link),
            ]
        );
        assert_eq!(named("(https://a.io)"), [("https://a.io", Mark::Link)]);
        // An address keeps its underscores and stars; nothing inside it is italic.
        assert_eq!(named("https://a.io/x_y_z*w*"), [("https://a.io/x_y_z*w*", Mark::Link)]);
    }

    #[test]
    fn a_fence_is_a_line_of_its_own() {
        assert!(has_fence("look:\n```\nx\n```"));
        assert!(has_fence("  ~~~py"));
        assert!(!has_fence("inline `code` only, and a ``` in a sentence"));
    }

    #[test]
    fn fenced_blocks_with_a_language() {
        let text = "a\n```TypeScript\nconst x = 1;\n```\n```\nplain\n```\n~~~py\nprint(1)";
        let found = fences(text);
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].0.as_str(), &text[found[0].1.clone()]), ("typescript", "const x = 1;\n"));
        assert_eq!((found[1].0.as_str(), &text[found[1].1.clone()]), ("py", "print(1)"));
    }

    #[test]
    fn plain_text_and_snake_case_are_left_alone() {
        assert_eq!(named("rename some_var_name to x * y"), []);
        assert_eq!(named("#hashtag and 3.5 apples"), []);
        assert_eq!(named("a lone ` backtick"), []);
    }
}
