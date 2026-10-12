//! File diffs for the chat: what an Edit, MultiEdit or Write tool call changed.
//!
//! Claude Code reports the change itself in the tool result (`tool_use_result.structuredPatch`
//! for an edit, `type: "create"` plus `content` for a new file). Before the result arrives, or
//! when it carries no patch, the diff is computed from the tool input (`old_string` and
//! `new_string`).

use std::path::PathBuf;

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// First line of the hunk in the old file, 1-based (0 for a new file).
    pub old_start: usize,
    /// First line of the hunk in the new file, 1-based.
    pub new_start: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub path: PathBuf,
    /// The file did not exist before.
    pub created: bool,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    pub fn added(&self) -> usize {
        self.count(LineKind::Added)
    }

    pub fn removed(&self) -> usize {
        self.count(LineKind::Removed)
    }

    fn count(&self, kind: LineKind) -> usize {
        self.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == kind)
            .count()
    }
}

/// Whether a tool changes files (and so gets a diff).
pub fn is_edit_tool(name: &str) -> bool {
    matches!(name, "Edit" | "MultiEdit" | "Write" | "NotebookEdit")
}

/// The diff of a finished tool call, from its result when it has one, else from its input.
pub fn from_tool(name: &str, input: &Value, result: Option<&Value>) -> Option<FileDiff> {
    if !is_edit_tool(name) {
        return None;
    }
    if let Some(diff) = result.and_then(from_result) {
        return Some(diff);
    }
    from_input(name, input)
}

/// From `tool_use_result`: a `structuredPatch` or a created file's `content`.
pub fn from_result(result: &Value) -> Option<FileDiff> {
    let path = PathBuf::from(result.get("filePath")?.as_str()?);
    if result.get("type").and_then(Value::as_str) == Some("create") {
        let content = result.get("content")?.as_str()?;
        return Some(created(path, content));
    }
    let patch = result.get("structuredPatch")?.as_array()?;
    if patch.is_empty() {
        return None;
    }
    let hunks = patch
        .iter()
        .map(|h| Hunk {
            old_start: h.get("oldStart").and_then(Value::as_u64).unwrap_or(0) as usize,
            new_start: h.get("newStart").and_then(Value::as_u64).unwrap_or(0) as usize,
            lines: h
                .get("lines")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(patch_line)
                .collect(),
        })
        .collect();
    Some(FileDiff {
        path,
        created: false,
        hunks,
    })
}

fn patch_line(line: &str) -> DiffLine {
    let (kind, text) = match line.chars().next() {
        Some('+') => (LineKind::Added, &line[1..]),
        Some('-') => (LineKind::Removed, &line[1..]),
        Some(' ') => (LineKind::Context, &line[1..]),
        _ => (LineKind::Context, line),
    };
    DiffLine {
        kind,
        text: text.to_string(),
    }
}

/// From the tool input alone: `old_string` / `new_string` pairs, or a Write's `content`.
pub fn from_input(name: &str, input: &Value) -> Option<FileDiff> {
    let path = PathBuf::from(
        input
            .get("file_path")
            .or_else(|| input.get("notebook_path"))?
            .as_str()?,
    );
    match name {
        "Write" => Some(created(path, input.get("content")?.as_str()?)),
        "Edit" => {
            let old = input.get("old_string")?.as_str()?;
            let new = input.get("new_string")?.as_str()?;
            Some(FileDiff {
                path,
                created: false,
                hunks: vec![text_hunk(old, new)],
            })
        }
        "MultiEdit" => {
            let hunks: Vec<Hunk> = input
                .get("edits")?
                .as_array()?
                .iter()
                .filter_map(|e| {
                    Some(text_hunk(
                        e.get("old_string")?.as_str()?,
                        e.get("new_string")?.as_str()?,
                    ))
                })
                .collect();
            Some(FileDiff {
                path,
                created: false,
                hunks,
            })
        }
        _ => None,
    }
}

fn created(path: PathBuf, content: &str) -> FileDiff {
    let lines = content
        .lines()
        .map(|l| DiffLine {
            kind: LineKind::Added,
            text: l.to_string(),
        })
        .collect();
    FileDiff {
        path,
        created: true,
        hunks: vec![Hunk {
            old_start: 0,
            new_start: 1,
            lines,
        }],
    }
}

/// A line diff of two snippets (line numbers unknown, so both start at 0).
pub fn text_hunk(old: &str, new: &str) -> Hunk {
    let diff = similar::TextDiff::from_lines(old, new);
    let lines = diff
        .iter_all_changes()
        .map(|c| DiffLine {
            kind: match c.tag() {
                similar::ChangeTag::Equal => LineKind::Context,
                similar::ChangeTag::Insert => LineKind::Added,
                similar::ChangeTag::Delete => LineKind::Removed,
            },
            text: c.value().trim_end_matches(['\n', '\r']).to_string(),
        })
        .collect();
    Hunk {
        old_start: 0,
        new_start: 0,
        lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn structured_patch() {
        let result = json!({
            "filePath": "/p/greet.py",
            "structuredPatch": [{"oldStart": 1, "oldLines": 2, "newStart": 1, "newLines": 2,
                "lines": [" def greet(name):", "-    return \"hello \" + name", "+    return \"hi \" + name"]}]
        });
        let diff = from_tool("Edit", &json!({}), Some(&result)).unwrap();
        assert_eq!(diff.path, PathBuf::from("/p/greet.py"));
        assert!(!diff.created);
        assert_eq!(diff.hunks[0].old_start, 1);
        let kinds: Vec<_> = diff.hunks[0].lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [LineKind::Context, LineKind::Removed, LineKind::Added]
        );
        assert_eq!(diff.hunks[0].lines[2].text, "    return \"hi \" + name");
        assert_eq!((diff.added(), diff.removed()), (1, 1));
    }

    #[test]
    fn created_file() {
        let result = json!({"type": "create", "filePath": "/p/n.md", "content": "# N\n\n- a\n", "structuredPatch": []});
        let diff = from_tool("Write", &json!({}), Some(&result)).unwrap();
        assert!(diff.created);
        assert_eq!(diff.added(), 3);
    }

    #[test]
    fn from_edit_input_without_result() {
        let input =
            json!({"file_path": "/p/a.rs", "old_string": "a\nb\nc", "new_string": "a\nB\nc"});
        let diff = from_tool("Edit", &input, None).unwrap();
        let lines: Vec<_> = diff.hunks[0]
            .lines
            .iter()
            .map(|l| (l.kind, l.text.as_str()))
            .collect();
        assert_eq!(
            lines,
            [
                (LineKind::Context, "a"),
                (LineKind::Removed, "b"),
                (LineKind::Added, "B"),
                (LineKind::Context, "c")
            ]
        );
    }

    #[test]
    fn multi_edit_input() {
        let input = json!({"file_path": "/p/a", "edits": [{"old_string": "x", "new_string": "y"}, {"old_string": "1", "new_string": "2"}]});
        let diff = from_input("MultiEdit", &input).unwrap();
        assert_eq!(diff.hunks.len(), 2);
        assert_eq!((diff.added(), diff.removed()), (2, 2));
    }

    #[test]
    fn other_tools_have_no_diff() {
        assert_eq!(from_tool("Read", &json!({"file_path": "/x"}), None), None);
    }
}
