//! Long pastes in a message. The message box keeps a long paste out of the way as a token
//! (`[Pasted text #1 · 120 lines]`), expanded to the full text when the message is sent. Claude
//! Code records a pasted block in the session file between markers
//! (`<pasted_content id="6f3e">…</pasted_content id="6f3e">`); the chat shows it folded.

/// A paste is long from this many lines, or this many characters.
pub const LONG_LINES: usize = 10;
pub const LONG_CHARS: usize = 1000;

const TOKEN: &str = "[Pasted text #";
const OPEN: &str = "<pasted_content";
const CLOSE: &str = "</pasted_content";

pub fn is_long(text: &str) -> bool {
    text.lines().count() >= LONG_LINES || text.chars().count() >= LONG_CHARS
}

/// What a paste is called, by its size: `120 lines`, or `1,450 characters` for few lines.
pub fn size(text: &str) -> String {
    let lines = text.lines().count();
    if lines >= LONG_LINES {
        format!("{lines} lines")
    } else {
        format!("{} characters", thousands(text.chars().count()))
    }
}

fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The token for paste `n` (from 1) in the message box.
pub fn token(n: usize, text: &str) -> String {
    format!("{TOKEN}{n} · {}]", size(text))
}

/// The message with each token replaced by its paste (`pastes[n - 1]`), between Claude Code's
/// markers when `marked` (so the chat folds it; [`without_markers`] gives the plain message). A
/// token without a paste stays as typed.
pub fn expand(message: &str, pastes: &[String], marked: bool) -> String {
    let mut out = String::new();
    let mut rest = message;
    while let Some(i) = rest.find(TOKEN) {
        out.push_str(&rest[..i]);
        let after = &rest[i + TOKEN.len()..];
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        let paste = digits.parse::<usize>().ok().and_then(|n| n.checked_sub(1)).and_then(|k| pastes.get(k));
        match (paste, after.find(']')) {
            (Some(p), Some(end)) if end >= digits.len() => {
                if marked {
                    out.push_str(&format!("{OPEN} id=\"k{digits}\">{p}{CLOSE} id=\"k{digits}\">"));
                } else {
                    out.push_str(p);
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push_str(TOKEN);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The numbers of the tokens left in the message box.
pub fn tokens_in(message: &str) -> Vec<usize> {
    message
        .match_indices(TOKEN)
        .filter_map(|(i, _)| message[i + TOKEN.len()..].chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok())
        .collect()
}

/// What a paste is about, for its card: a web page's title, else its first heading, else its
/// first line, without Markdown's marks, cut at about 80 characters.
pub fn heading(text: &str) -> String {
    if let Some((title, _)) = text.strip_prefix("[Web page: ").and_then(|t| t.split_once("]\n")) {
        return cut(title.trim(), 80);
    }
    let lines = || text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("```"));
    let line = lines().find(|l| l.starts_with('#')).or_else(|| lines().next()).unwrap_or("");
    let plain = line.trim_start_matches(['#', '>', '-', '*', ' ']).replace("**", "").replace('`', "");
    cut(plain.trim(), 80)
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let short: String = text.chars().take(max).collect();
    format!("{}…", short.trim_end())
}

/// What kind of text a paste is, as its card says it: a web page, JSON, Markdown, code or text.
pub fn kind(text: &str) -> &'static str {
    let t = text.trim();
    if t.starts_with("[Web page: ") {
        return "Web page";
    }
    if (t.starts_with('{') || t.starts_with('[')) && serde_json::from_str::<serde_json::Value>(t).is_ok() {
        return "JSON";
    }
    let lines: Vec<&str> = t.lines().map(str::trim_start).collect();
    let markdown = lines.iter().filter(|l| l.starts_with("# ") || l.starts_with("## ") || l.starts_with("### ") || l.starts_with("```")).count();
    if markdown > 0 {
        return "Markdown";
    }
    let code = lines.iter().filter(|l| l.ends_with(['{', '}', ';']) || l.starts_with("fn ") || l.starts_with("def ") || l.starts_with("import ")).count();
    if code * 3 >= lines.len().max(1) { "Code" } else { "Text" }
}

/// A paste's size in bytes, as people read it: `840 B`, `5.6 KB`, `1.2 MB`.
pub fn bytes(text: &str) -> String {
    let n = text.len() as f64;
    if n < 1000. {
        format!("{n} B")
    } else if n < 1_000_000. {
        format!("{:.1} KB", n / 1000.)
    } else {
        format!("{:.1} MB", n / 1_000_000.)
    }
}

/// A stretch of a message: typed text, or a pasted block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part<'a> {
    Text(&'a str),
    Pasted(&'a str),
}

/// A message cut at Claude Code's paste markers. Unmatched markers stay as text.
pub fn parts(message: &str) -> Vec<Part<'_>> {
    let mut out = Vec::new();
    let mut rest = message;
    while let Some(i) = rest.find(OPEN) {
        let Some(open_end) = rest[i..].find('>').map(|j| i + j + 1) else { break };
        let Some(close) = rest[open_end..].find(CLOSE).map(|j| open_end + j) else { break };
        let close_end = rest[close..].find('>').map_or(rest.len(), |j| close + j + 1);
        if i > 0 {
            out.push(Part::Text(&rest[..i]));
        }
        out.push(Part::Pasted(rest[open_end..close].trim_matches('\n')));
        rest = &rest[close_end..];
    }
    if !rest.is_empty() {
        out.push(Part::Text(rest));
    }
    out
}

/// The message without paste markers, everything else (the pasted text too) as it was.
pub fn without_markers(message: &str) -> String {
    let mut out = String::new();
    let mut rest = message;
    while let Some(i) = [OPEN, CLOSE].iter().filter_map(|m| rest.find(m)).min() {
        let Some(end) = rest[i..].find('>') else { break };
        out.push_str(&rest[..i]);
        rest = &rest[i + end + 1..];
    }
    out.push_str(rest);
    out
}

/// A message as a title: what was typed, the pastes left out (a message of only a paste: its
/// first line).
pub fn title_text(message: &str) -> String {
    let parts = parts(message);
    let typed: Vec<&str> = parts
        .iter()
        .filter_map(|p| match p {
            Part::Text(t) if !t.trim().is_empty() => Some(t.trim()),
            _ => None,
        })
        .collect();
    if !typed.is_empty() {
        return typed.join(" ");
    }
    parts
        .iter()
        .find_map(|p| match p {
            Part::Pasted(t) => t.lines().find(|l| !l.trim().is_empty()).map(str::trim),
            _ => None,
        })
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_is_ten_lines_or_a_thousand_characters() {
        assert!(!is_long("short"));
        assert!(!is_long(&"line\n".repeat(9)));
        assert!(is_long(&"line\n".repeat(10)));
        assert!(is_long(&"x".repeat(1000)));
        assert_eq!(size(&"line\n".repeat(120)), "120 lines");
        assert_eq!(size(&"x".repeat(1450)), "1,450 characters");
    }

    #[test]
    fn tokens_expand_in_place_and_a_deleted_one_drops_its_paste() {
        let pastes = vec!["ERROR 1".to_string(), "ERROR 2".to_string()];
        let msg = format!("here: {} and {} why?", token(1, &pastes[0]), token(2, &pastes[1]));
        assert_eq!(expand(&msg, &pastes, false), "here: ERROR 1 and ERROR 2 why?");
        assert_eq!(tokens_in(&msg), [1, 2]);
        let msg = format!("only {}", token(2, &pastes[1]));
        assert_eq!(expand(&msg, &pastes, false), "only ERROR 2");
        // Without its paste, or half a token, it stays as typed.
        assert_eq!(expand("see [Pasted text #9 · 3 lines]", &pastes, false), "see [Pasted text #9 · 3 lines]");
        assert_eq!(expand("see [Pasted text #", &pastes, false), "see [Pasted text #");
        // Marked, it folds in the chat and unmarks to exactly what is sent.
        let msg = format!("here: {} why?", token(1, &pastes[0]));
        let marked = expand(&msg, &pastes, true);
        assert_eq!(parts(&marked), [Part::Text("here: "), Part::Pasted("ERROR 1"), Part::Text(" why?")]);
        assert_eq!(without_markers(&marked), expand(&msg, &pastes, false));
    }

    #[test]
    fn a_paste_is_named_by_its_heading_and_kind() {
        let doc = "Some intro\n\n# Making sure agents **always** know about `MCP` tools\n\n- one\n";
        assert_eq!(heading(doc), "Making sure agents always know about MCP tools");
        assert_eq!(kind(doc), "Markdown");
        assert_eq!(heading("\n  $ make\nerror\n"), "$ make");
        assert_eq!(heading("[Web page: Example Domain]\n[https://example.com/]\n\ntext"), "Example Domain");
        assert_eq!(kind("[Web page: Example]\n[u]\n\nx"), "Web page");
        assert_eq!(kind("{\"a\": [1, 2]}"), "JSON");
        assert_eq!(kind("fn main() {\n    run();\n}\n"), "Code");
        assert_eq!(kind("just some words\nand more words\n"), "Text");
        assert!(heading(&"word ".repeat(40)).ends_with('…'));
        assert_eq!((bytes("abc"), bytes(&"x".repeat(5600))), ("3 B".to_string(), "5.6 KB".to_string()));
    }

    #[test]
    fn claude_code_markers_split_a_message() {
        let msg = "I ran this:\n\n<pasted_content id=\"6f3e\">\n$ make\nerror\n</pasted_content id=\"6f3e\">\n\nwhy?";
        assert_eq!(parts(msg), [Part::Text("I ran this:\n\n"), Part::Pasted("$ make\nerror"), Part::Text("\n\nwhy?")]);
        assert_eq!(without_markers(msg).split_whitespace().collect::<Vec<_>>(), ["I", "ran", "this:", "$", "make", "error", "why?"]);
        assert_eq!(parts("plain"), [Part::Text("plain")]);
        assert_eq!(title_text(msg), "I ran this: why?");
        assert_eq!(title_text("<pasted_content id=\"x\">\n\n$ make\n</pasted_content id=\"x\">"), "$ make");
        assert_eq!(parts("<pasted_content id=\"x\"> no end"), [Part::Text("<pasted_content id=\"x\"> no end")]);
    }
}
