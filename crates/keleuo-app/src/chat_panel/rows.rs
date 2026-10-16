//! The list's rows. Most items are one row each; a long reply is several, a piece of a few
//! paragraphs each. GPUI's list draws only the rows in view, but a row is drawn and laid out
//! whole: a reply of 40 KB as one row was laid out whole on every frame of its streaming, the
//! part out of view included, and each frame cost more as it grew. In pieces, only the pieces in
//! view are.

use std::collections::HashMap;
use std::ops::Range;

use keleuo_core::conversation::Item;

/// A reply is split from this length...
const SPLIT_FROM: usize = 3_000;
/// ...into pieces of about this length (each ends with a paragraph).
const PIECE: usize = 1_500;

/// One row of the list: item `item`'s piece `part` (of `of`), `range` of its text when a reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Row {
    pub item: usize,
    pub part: usize,
    pub of: usize,
    pub range: Range<usize>,
}

impl Row {
    pub fn whole(item: usize) -> Self {
        Self { item, part: 0, of: 1, range: 0..0 }
    }

    /// A piece of a reply split in several.
    pub fn is_piece(&self) -> bool {
        self.of > 1
    }
}

/// Where a reply's text splits: after a paragraph (a blank line outside a code block, the next
/// line not indented, so a list's or a block's continuation stays with it), once a piece is long
/// enough. Decided from the start of the text only, so as a reply grows its earlier pieces
/// stay as they were and only the last grows or new ones follow.
pub(super) fn reply_pieces(text: &str) -> Vec<Range<usize>> {
    if text.len() < SPLIT_FROM {
        // One piece: the whole text (a list of one range, not the numbers in it).
        #[allow(clippy::single_range_in_vec_init)]
        return vec![0..text.len()];
    }
    let mut out = Vec::new();
    let (mut start, mut pos) = (0, 0);
    let (mut fence, mut after_blank) = (false, false);
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let indented = line.starts_with([' ', '\t']);
        if !fence && after_blank && !trimmed.is_empty() && !indented && pos - start >= PIECE {
            out.push(start..pos);
            start = pos;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = !fence;
        }
        after_blank = line.trim().is_empty();
        pos += line.len();
    }
    out.push(start..text.len());
    out
}

/// The rows for `items`, each reply's pieces kept in `cache` by its length (only a reply that
/// grew is split again).
pub(super) type PieceCache = HashMap<usize, (usize, u64, Vec<Range<usize>>)>;

/// A cheap check that the reply at an index is still the one split (its start).
fn start_of(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.as_bytes()[..text.len().min(256)].hash(&mut h);
    h.finish()
}

pub(super) fn rows(items: &[Item], cache: &mut PieceCache) -> Vec<Row> {
    let mut out = Vec::with_capacity(items.len());
    for (ix, item) in items.iter().enumerate() {
        match item {
            Item::Assistant { text, .. } if text.len() >= SPLIT_FROM => {
                let start = start_of(text);
                let pieces = match cache.get(&ix) {
                    Some((len, s, pieces)) if *len == text.len() && *s == start => pieces.clone(),
                    _ => {
                        let pieces = reply_pieces(text);
                        cache.insert(ix, (text.len(), start, pieces.clone()));
                        pieces
                    }
                };
                let of = pieces.len();
                out.extend(pieces.into_iter().enumerate().map(|(part, range)| Row { item: ix, part, of, range }));
            }
            _ => out.push(Row::whole(ix)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(n: usize) -> String {
        format!("{}\n\n", "word ".repeat(n))
    }

    #[test]
    fn a_short_reply_is_one_piece() {
        assert_eq!(reply_pieces("hello\n\nworld"), vec![0..12]);
    }

    #[test]
    fn a_long_reply_splits_after_paragraphs_and_keeps_its_pieces_as_it_grows() {
        let text: String = (0..20).map(|_| para(60)).collect();
        let pieces = reply_pieces(&text);
        assert!(pieces.len() > 2);
        // Every piece but the last ends with a blank line; together they are the whole text.
        assert_eq!(pieces.last().unwrap().end, text.len());
        for w in pieces.windows(2) {
            assert_eq!(w[0].end, w[1].start);
            assert!(text[w[0].clone()].ends_with("\n\n"));
        }
        // Grown: the pieces before the last are the same.
        let more = format!("{text}{}", para(400));
        let grown = reply_pieces(&more);
        assert_eq!(&grown[..pieces.len() - 1], &pieces[..pieces.len() - 1]);
    }

    #[test]
    fn code_blocks_and_indented_continuations_are_not_split() {
        let mut text = para(400);
        text.push_str("```\n");
        for _ in 0..200 {
            text.push_str("let x = 1;\n\n");
        }
        text.push_str("```\n");
        text.push_str(&para(400));
        for piece in reply_pieces(&text) {
            let fences = text[piece].matches("```").count();
            assert_eq!(fences % 2, 0, "a code block stays in one piece");
        }
        let list = format!("{}- item\n\n    more of the item\n\n{}", para(400), para(400));
        for piece in reply_pieces(&list) {
            assert!(!text_starts_indented(&list[piece]));
        }
    }

    fn text_starts_indented(s: &str) -> bool {
        s.starts_with([' ', '\t'])
    }
}
