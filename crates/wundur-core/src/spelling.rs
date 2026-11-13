//! Spelling in the message box: which words of a message are worth checking. Code (inline and
//! fenced), web addresses, file paths, `/commands`, `@mentions` and identifiers are left
//! alone, as a native text field would leave them to a code editor. Asking the system's
//! checker is the window's part; this only says where to look, and in what pieces.

use std::ops::Range;

use crate::markdown_marks::{Mark, marks};

#[cfg(any(test, not(any(target_os = "macos", target_os = "windows"))))]
pub mod hunspell;

/// The longest piece of text asked about at once: a line, or a long line cut at a space.
pub const MAX_CHUNK: usize = 4096;

/// The words of `text` to check, as UTF-8 byte ranges in order. `skip` holds ranges to leave
/// alone besides what Markdown says is code (the message box's paste pills).
pub fn words(text: &str, skip: &[Range<usize>]) -> Vec<Range<usize>> {
    // Code, Markdown's own characters and bare addresses become spaces, so what is left
    // splits into words around them at the same offsets.
    let mut masked = text.as_bytes().to_vec();
    let mut blank = |range: Range<usize>| masked[range.start.min(text.len())..range.end.min(text.len())].fill(b' ');
    for range in skip {
        blank(range.clone());
    }
    for (range, mark) in marks(text) {
        let address = mark == Mark::Link && text[range.clone()].starts_with("http");
        if matches!(mark, Mark::Code | Mark::Syntax) || address {
            blank(range);
        }
    }
    // Whole characters were blanked (the ranges fall on character boundaries), so it is
    // still UTF-8.
    let masked = String::from_utf8(masked).unwrap_or_default();
    let mut out = Vec::new();
    let mut at = 0;
    for token in masked.split(|c: char| c.is_whitespace()) {
        let start = at;
        at += token.len() + masked[at + token.len()..].chars().next().map_or(0, char::len_utf8);
        if !token.is_empty() && !is_name(token) {
            token_words(token, start, &mut out);
        }
    }
    out
}

/// Whether a run of text between spaces names something rather than saying it: a path or an
/// address (`/`, `\`), a `/command`, an `@mention` or an email, a file or a domain (`foo.rs`,
/// `example.com`), a command's flag (`-v`, `--release`) or a variable (`$HOME`).
fn is_name(token: &str) -> bool {
    let b = token.as_bytes();
    let dotted = b.windows(3).any(|w| w[1] == b'.' && w[0].is_ascii_alphanumeric() && w[2].is_ascii_alphanumeric());
    let flag = token.starts_with('-') && token[1..].starts_with(|c: char| c == '-' || c.is_alphanumeric());
    token.contains(['/', '\\', '@']) || token.starts_with('$') || dotted || flag
}

/// The words of one token (`start` is where it begins in the message): runs of letters with
/// apostrophes inside them, those with digits or underscores, capitals inside (`camelCase`) or
/// in capitals throughout (`HTTP`) left out, as are single letters.
fn token_words(token: &str, start: usize, out: &mut Vec<Range<usize>>) {
    let chars: Vec<(usize, char)> = token.char_indices().collect();
    let part = |i: usize| {
        let c = chars[i].1;
        c.is_alphanumeric()
            || c == '_'
            || (matches!(c, '\'' | '’')
                && i > 0
                && chars[i - 1].1.is_alphanumeric()
                && chars.get(i + 1).is_some_and(|n| n.1.is_alphanumeric()))
    };
    let mut i = 0;
    while i < chars.len() {
        if !part(i) {
            i += 1;
            continue;
        }
        let from = i;
        while i < chars.len() && part(i) {
            i += 1;
        }
        let word = &token[chars[from].0..chars.get(i).map_or(token.len(), |c| c.0)];
        let letters = word.chars().filter(|c| c.is_alphabetic()).count();
        let identifier = word.contains(|c: char| c.is_numeric() || c == '_');
        let capitals = word.chars().skip(1).any(char::is_uppercase);
        if letters >= 2 && !identifier && !capitals {
            out.push(start + chars[from].0..start + chars[from].0 + word.len());
        }
    }
}

/// The word being typed, among `words`: the one the cursor is in or at the end of. It is not
/// checked until the cursor leaves it.
pub fn typing(words: &[Range<usize>], cursor: usize) -> Option<usize> {
    words.iter().position(|w| w.start <= cursor && cursor <= w.end)
}

/// The pieces to ask the checker about, as byte ranges: each line (without its line break),
/// long ones cut at a space so no piece is over [`MAX_CHUNK`]. A piece that has not changed
/// need not be asked about again.
pub fn chunks(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let mut rest = start..start + line.trim_end_matches(['\n', '\r']).len();
        while rest.len() > MAX_CHUNK {
            let mut cut = rest.start + MAX_CHUNK;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            // At the last space before the limit, or at the limit for a line without one.
            let cut = text[rest.start..cut].rfind(char::is_whitespace).map_or(cut, |s| rest.start + s + 1);
            out.push(rest.start..cut);
            rest.start = cut;
        }
        if !rest.is_empty() {
            out.push(rest);
        }
    }
    out
}

/// The UTF-8 offset in `text` of a UTF-16 offset (the systems' checkers count in UTF-16).
pub fn utf8_offset(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (i, c) in text.char_indices() {
        if units >= utf16 {
            return i;
        }
        units += c.len_utf16();
    }
    text.len()
}

/// The UTF-16 offset in `text` of a UTF-8 offset.
pub fn utf16_offset(text: &str, utf8: usize) -> usize {
    text[..utf8.min(text.len())].encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(text: &str) -> Vec<&str> {
        words(text, &[]).into_iter().map(|r| &text[r]).collect()
    }

    #[test]
    fn plain_words_are_checked() {
        assert_eq!(checked("Thiss is a tset, isn't it?"), ["Thiss", "is", "tset", "isn't", "it"]);
        assert_eq!(checked("well-knwn café"), ["well", "knwn", "café"]);
        assert_eq!(checked("“quoted” (words)."), ["quoted", "words"]);
    }

    #[test]
    fn code_is_left_alone() {
        assert_eq!(checked("run `cargo biuld` now"), ["run", "now"]);
        assert_eq!(checked("look:\n```rust\nlet x = fooo();\n```\ndone"), ["look", "done"]);
        // An open fence: what follows is code until it closes.
        assert_eq!(checked("see\n```\nunfinishd"), ["see"]);
    }

    #[test]
    fn names_are_left_alone() {
        assert_eq!(checked("open src/main.rs or C:\\dir\\x and foo.rs"), ["open", "or", "and"]);
        assert_eq!(checked("/compct then @someone mail a@b.com"), ["then", "mail"]);
        assert_eq!(checked("see https://exmple.com/x and examle.org."), ["see", "and"]);
        assert_eq!(checked("pass --relese or -rf with $HOMEE"), ["pass", "or", "with"]);
    }

    #[test]
    fn identifiers_are_left_alone() {
        assert_eq!(checked("call getUserNme and FooBarr"), ["call", "and"]);
        assert_eq!(checked("the HTTPS and JSONN keys"), ["the", "and", "keys"]);
        assert_eq!(checked("snake_cse and v2beta and x"), ["and", "and"]);
    }

    #[test]
    fn link_text_is_checked_but_not_its_address() {
        assert_eq!(checked("a [nice lnk](https://x.io/pth) here"), ["nice", "lnk", "here"]);
    }

    #[test]
    fn skipped_ranges_are_left_alone() {
        let text = "hello [Pasted text #1 · 2 KB] wrld";
        let pill = 6..text.find(']').unwrap() + 1;
        let found: Vec<&str> = words(text, &[pill]).into_iter().map(|r| &text[r]).collect();
        assert_eq!(found, ["hello", "wrld"]);
    }

    #[test]
    fn the_word_being_typed_waits() {
        let text = "a mispeled wor";
        let w = words(text, &[]);
        assert_eq!(typing(&w, text.len()).map(|i| &text[w[i].clone()]), Some("wor"));
        assert_eq!(typing(&w, 5).map(|i| &text[w[i].clone()]), Some("mispeled"));
        // Past the space after a word, it is done.
        let done = "mispeled ";
        assert_eq!(typing(&words(done, &[]), done.len()), None);
    }

    #[test]
    fn lines_are_the_pieces_and_long_ones_are_cut() {
        let text = "one\n\ntwo\r\nthree";
        let pieces: Vec<&str> = chunks(text).into_iter().map(|r| &text[r]).collect();
        assert_eq!(pieces, ["one", "two", "three"]);
        let long = "word ".repeat(2000);
        let pieces = chunks(&long);
        assert!(pieces.iter().all(|r| r.len() <= MAX_CHUNK));
        assert_eq!(pieces.first().map(|r| r.start), Some(0));
        assert_eq!(pieces.last().map(|r| r.end), Some(long.len()));
        assert!(pieces.windows(2).all(|p| p[0].end == p[1].start && long[..p[0].end].ends_with(' ')));
    }

    #[test]
    fn utf16_offsets_convert() {
        let text = "é😀a";
        assert_eq!(utf16_offset(text, text.len()), 4);
        assert_eq!(utf8_offset(text, 1), 2);
        assert_eq!(utf8_offset(text, 3), 6);
        assert_eq!(utf8_offset(text, 9), text.len());
    }
}
