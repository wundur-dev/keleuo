//! A table from a reply, taken out of the chat: as CSV for a spreadsheet, or as Markdown with
//! Keleuo's own file links taken back out.

use crate::links::SCHEME;

/// The table as CSV (RFC 4180): a field with a comma, a quote, a line break or space at
/// either end is quoted, its quotes doubled; lines end with CRLF. A byte order mark comes
/// first, so Excel reads the text as UTF-8 rather than the system's code page.
pub fn to_csv(headers: &[String], rows: &[Vec<String>]) -> String {
    let mut out = String::from("\u{feff}");
    for row in std::iter::once(headers).chain(rows.iter().map(Vec::as_slice)) {
        let fields: Vec<String> = row.iter().map(|f| csv_field(f)).collect();
        out.push_str(&fields.join(","));
        out.push_str("\r\n");
    }
    out
}

fn csv_field(field: &str) -> String {
    let quote = field.contains([',', '"', '\n', '\r'])
        || field.starts_with(char::is_whitespace)
        || field.ends_with(char::is_whitespace);
    if quote { format!("\"{}\"", field.replace('"', "\"\"")) } else { field.to_string() }
}

/// `markdown` with each `[text](keleuo-open:…)` link that Keleuo added for a file reference
/// put back to its text, so a copied table reads as the agent wrote it. Other links stay.
pub fn without_file_links(markdown: &str) -> String {
    let marker = format!("]({SCHEME}");
    let mut out = String::with_capacity(markdown.len());
    let mut rest = markdown;
    while let Some(at) = rest.find(&marker) {
        let before = &rest[..at];
        let after = &rest[at + marker.len()..];
        match (before.rfind('['), after.find(')')) {
            (Some(open), Some(close)) => {
                out.push_str(&before[..open]);
                out.push_str(&before[open + 1..]);
                rest = &after[close + 1..];
            }
            // Not a whole link: keep the text as it is.
            _ => {
                out.push_str(&rest[..at + marker.len()]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn csv_quotes_only_what_needs_it() {
        let csv = to_csv(&s(&["Name", "Total"]), &[s(&["Acme, Inc.", "1,234"]), s(&["say \"hi\"", "12%"]), s(&["plain", " padded"])]);
        assert_eq!(
            csv,
            "\u{feff}Name,Total\r\n\"Acme, Inc.\",\"1,234\"\r\n\"say \"\"hi\"\"\",12%\r\nplain,\" padded\"\r\n"
        );
    }

    #[test]
    fn csv_keeps_ragged_rows_and_line_breaks() {
        let csv = to_csv(&s(&["a", "b", "c"]), &[s(&["1"]), s(&["two\nlines", "", "3"])]);
        assert_eq!(csv, "\u{feff}a,b,c\r\n1\r\n\"two\nlines\",,3\r\n");
    }

    #[test]
    fn file_links_go_back_to_their_text() {
        let md = format!("| File | Note |\n| :-- | :-- |\n| [`src/main.rs`]({SCHEME}src/main.rs#L4) | see [docs](https://example.com) |");
        assert_eq!(
            without_file_links(&md),
            "| File | Note |\n| :-- | :-- |\n| `src/main.rs` | see [docs](https://example.com) |"
        );
    }

    #[test]
    fn text_without_links_is_unchanged() {
        assert_eq!(without_file_links("| a | b |"), "| a | b |");
        assert_eq!(without_file_links(&format!("odd ]({SCHEME}x")), format!("odd ]({SCHEME}x"));
    }
}
