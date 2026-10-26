//! Searching a conversation: the text of each item as it reads (a reply's Markdown, a tool
//! call's summary and output, a summary's lines), and where a query occurs in it. Searches the
//! data, not what is drawn: an item out of view, or in a part of the session not read yet,
//! is found the same.

use std::borrow::Cow;

use crate::conversation::Item;
use crate::summary;

/// What a search looks through in `item`.
pub fn item_text(item: &Item) -> Cow<'_, str> {
    match item {
        Item::User(t) | Item::Notice(t) | Item::Error(t) => Cow::Borrowed(t),
        Item::Assistant { text, .. } => Cow::Borrowed(text),
        Item::Tool(t) => {
            let mut s = format!("{} {}", t.name, t.summary);
            if let Some(r) = &t.result {
                s.push('\n');
                s.push_str(r);
            }
            Cow::Owned(s)
        }
        Item::Permission(p) => Cow::Owned(format!("{} {}\n{}", p.tool_name, p.summary, p.description.as_deref().unwrap_or_default())),
        Item::Compacted { summary, .. } => Cow::Borrowed(summary),
        Item::AgentMessage { title, text, .. } => Cow::Owned(format!("{title}\n{text}")),
        Item::Outgoing { to, text, .. } => Cow::Owned(format!("{to}\n{text}")),
        Item::ToolRun { tool, command, .. } => Cow::Owned(format!("{tool}\n{command}")),
        Item::SignIn(_) => Cow::Borrowed(""),
        Item::TurnChanges(changes) => {
            Cow::Owned(changes.iter().map(|c| c.path.display().to_string()).collect::<Vec<_>>().join("\n"))
        }
        Item::Summary { state: summary::State::Ready(s), .. } => {
            let mut lines = vec![s.headline.clone()];
            lines.extend(s.changed.iter().cloned());
            lines.extend(s.checked.iter().map(|c| c.what.clone()));
            lines.extend(s.needs_you.iter().map(|t| t.text.clone()));
            Cow::Owned(lines.join("\n"))
        }
        Item::Summary { .. } | Item::Images(_) | Item::Earlier { .. } => Cow::Borrowed(""),
    }
}

/// Where `query` occurs in `text`, ignoring case: the byte offset of each (in `text`).
pub fn occurrences(text: &str, query: &str) -> Vec<usize> {
    ranges(text, query).into_iter().map(|r| r.start).collect()
}

/// Where `query` occurs in `text`, ignoring case: byte ranges of `text`.
pub fn ranges(text: &str, query: &str) -> Vec<std::ops::Range<usize>> {
    let query = query.trim();
    if query.is_empty() || text.is_empty() {
        return Vec::new();
    }
    // Lowercased side by side, char by char, so offsets stay those of `text` (a lowercase
    // form can differ in length).
    let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let hay: Vec<(usize, usize, char)> =
        text.char_indices().flat_map(|(i, c)| c.to_lowercase().map(move |l| (i, i + c.len_utf8(), l))).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if hay[i..i + needle.len()].iter().map(|(_, _, c)| *c).eq(needle.iter().copied()) {
            out.push(hay[i].0..hay[i + needle.len() - 1].1);
            i += needle.len();
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_every_occurrence_ignoring_case() {
        assert_eq!(occurrences("Foo foo FOO", "foo"), vec![0, 4, 8]);
        assert_eq!(occurrences("Straße strasse", "STRASSE"), vec![8], "byte offsets: ß is two bytes");
        assert!(occurrences("anything", "  ").is_empty());
        assert!(ranges("a Straße", "STRASSE").is_empty(), "ß lowercases to itself, not ss");
        assert_eq!(ranges("Ab ab", "AB"), vec![0..2, 3..5]);
        assert_eq!(occurrences("añb añb", "ÑB"), vec![1, 6]);
    }

    #[test]
    fn a_tool_call_is_found_by_its_output_too() {
        use crate::agent::Event;
        let mut chat = crate::conversation::Conversation::default();
        chat.apply(Event::ToolCall { id: "t".into(), name: "Bash".into(), summary: "cargo test".into(), input: serde_json::json!({}), diff: None, parent: None });
        chat.apply(Event::ToolResult { id: "t".into(), output: "test result: ok".into(), is_error: false, diff: None, file_changed: None, parent: None });
        let text = item_text(chat.items.last().unwrap());
        assert_eq!(occurrences(&text, "result").len(), 1);
        assert_eq!(occurrences(&text, "cargo").len(), 1);
    }
}
