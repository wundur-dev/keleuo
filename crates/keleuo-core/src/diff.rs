//! File diffs for the chat: what an Edit, MultiEdit or Write tool call changed.
//!
//! Claude Code reports the change itself in the tool result (`tool_use_result.structuredPatch`
//! for an edit, `type: "create"` plus `content` for a new file). Before the result arrives, or
//! when it carries no patch, the diff is computed from the tool input (`old_string` and
//! `new_string`).
//!
//! The code viewer shows what a session changed in a file as a whole-file diff: the file before
//! the session ([`undo`] of its changes) against the file now ([`file_rows`]).

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
    // A Write over an existing file that changed nothing: no diff (its input alone would read
    // as a new file).
    if result.and_then(|r| r.get("type")).and_then(Value::as_str) == Some("update") {
        return None;
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

/// One row of a whole-file diff: a line of the file now, or a removed line where it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffRow {
    pub kind: LineKind,
    /// Line number in the old file (1-based), for context and removed lines.
    pub old: Option<usize>,
    /// Line number in the file now (1-based), for context and added lines.
    pub new: Option<usize>,
    pub text: String,
}

/// The whole of `new` against `old`, line by line, unchanged lines included. A very large
/// diff gives up on finding the smallest one after a second rather than stalling.
pub fn file_rows(old: &str, new: &str) -> Vec<DiffRow> {
    let diff = similar::TextDiff::configure()
        .timeout(std::time::Duration::from_secs(1))
        .diff_lines(old, new);
    diff.iter_all_changes()
        .map(|c| DiffRow {
            kind: match c.tag() {
                similar::ChangeTag::Equal => LineKind::Context,
                similar::ChangeTag::Insert => LineKind::Added,
                similar::ChangeTag::Delete => LineKind::Removed,
            },
            old: c.old_index().map(|i| i + 1),
            new: c.new_index().map(|i| i + 1),
            text: c.value().trim_end_matches(['\n', '\r']).to_string(),
        })
        .collect()
}

/// What `changes` (oldest first) did to a file, given its content now: the whole file as rows.
/// Line endings are compared as `\n`.
pub fn session_rows(current: &str, changes: &[FileDiff]) -> Vec<DiffRow> {
    let current = current.replace("\r\n", "\n");
    file_rows(&undo(&current, changes), &current)
}

/// Where each run of changed rows starts, to step from change to change.
pub fn change_starts(rows: &[DiffRow]) -> Vec<usize> {
    (0..rows.len())
        .filter(|&i| {
            rows[i].kind != LineKind::Context && (i == 0 || rows[i - 1].kind == LineKind::Context)
        })
        .collect()
}

/// The file as it was before `changes` (oldest first), given its content now: each change is
/// undone, newest first, by finding its new lines (near where the change says they are) and
/// putting its old lines back. A change that cannot be found (the file was changed some other
/// way since) is left as it is. Before a change that created the file there was nothing.
pub fn undo(current: &str, changes: &[FileDiff]) -> String {
    let mut lines: Vec<&str> = current.lines().collect();
    for change in changes.iter().rev() {
        if change.created {
            return String::new();
        }
        for hunk in change.hunks.iter().rev() {
            let new: Vec<&str> = hunk
                .lines
                .iter()
                .filter(|l| l.kind != LineKind::Removed)
                .map(|l| l.text.as_str())
                .collect();
            let old: Vec<&str> = hunk
                .lines
                .iter()
                .filter(|l| l.kind != LineKind::Added)
                .map(|l| l.text.as_str())
                .collect();
            if new == old {
                continue;
            }
            if let Some(at) = find_block(&lines, &new, hunk.new_start.checked_sub(1)) {
                lines.splice(at..at + new.len(), old);
            }
        }
    }
    let mut text = lines.join("\n");
    if !text.is_empty() && current.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// Where `block` occurs in `lines`, the occurrence nearest `hint` (0-based). An empty block (a
/// deletion with no context around it) can only be placed at the hint.
fn find_block(lines: &[&str], block: &[&str], hint: Option<usize>) -> Option<usize> {
    if block.is_empty() {
        return hint.filter(|&h| h <= lines.len());
    }
    let hint = hint.unwrap_or(0);
    if block.len() > lines.len() {
        return None;
    }
    let matches = |at: usize| {
        lines[at..at + block.len()]
            .iter()
            .map(|l| l.trim_end_matches('\r'))
            .eq(block.iter().copied())
    };
    (0..=lines.len() - block.len())
        .filter(|&at| matches(at))
        .min_by_key(|&at| at.abs_diff(hint))
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

    fn edit(path: &str, old: &str, new: &str) -> FileDiff {
        from_input(
            "Edit",
            &json!({"file_path": path, "old_string": old, "new_string": new}),
        )
        .unwrap()
    }

    fn kinds(rows: &[DiffRow]) -> Vec<(LineKind, Option<usize>, Option<usize>, &str)> {
        rows.iter()
            .map(|r| (r.kind, r.old, r.new, r.text.as_str()))
            .collect()
    }

    #[test]
    fn whole_file_rows_number_both_sides() {
        let rows = file_rows("a\nb\nc\n", "a\nB\nc\nd\n");
        assert_eq!(
            kinds(&rows),
            [
                (LineKind::Context, Some(1), Some(1), "a"),
                (LineKind::Removed, Some(2), None, "b"),
                (LineKind::Added, None, Some(2), "B"),
                (LineKind::Context, Some(3), Some(3), "c"),
                (LineKind::Added, None, Some(4), "d"),
            ]
        );
        assert_eq!(change_starts(&rows), [1, 4]);
    }

    #[test]
    fn undo_structured_patches_newest_first() {
        // Two edits as Claude Code reports them: line numbers in the file after each one.
        let first = from_result(
            &json!({"filePath": "/p/a", "structuredPatch": [{"oldStart": 1, "newStart": 1,
            "lines": [" one", "-two", "+TWO", " three"]}]}),
        )
        .unwrap();
        let second = from_result(
            &json!({"filePath": "/p/a", "structuredPatch": [{"oldStart": 3, "newStart": 3,
            "lines": [" three", "+three and a half", " four"]}]}),
        )
        .unwrap();
        let now = "one\nTWO\nthree\nthree and a half\nfour\n";
        assert_eq!(
            undo(now, &[first.clone(), second.clone()]),
            "one\ntwo\nthree\nfour\n"
        );
        let rows = session_rows(now, &[first, second]);
        assert_eq!(rows.iter().filter(|r| r.kind == LineKind::Added).count(), 2);
        assert_eq!(
            rows.iter().filter(|r| r.kind == LineKind::Removed).count(),
            1
        );
        assert_eq!(
            rows.iter().filter(|r| r.kind == LineKind::Context).count(),
            3
        );
    }

    #[test]
    fn undo_input_edits_without_line_numbers() {
        let changes = [
            edit("/p/a", "let x = 1;", "let x = 2;"),
            edit("/p/a", "fn f() {}", "fn f() {\n    g();\n}"),
        ];
        let now = "fn f() {\n    g();\n}\nlet x = 2;";
        assert_eq!(undo(now, &changes), "fn f() {}\nlet x = 1;");
    }

    #[test]
    fn undo_picks_the_occurrence_nearest_the_hint() {
        let change = from_result(
            &json!({"filePath": "/p/a", "structuredPatch": [{"oldStart": 3, "newStart": 3,
            "lines": ["-x", "+y"]}]}),
        )
        .unwrap();
        assert_eq!(undo("y\nz\ny\nz\n", &[change]), "y\nz\nx\nz\n");
    }

    #[test]
    fn undo_skips_a_change_no_longer_there() {
        let change = edit("/p/a", "old", "new");
        assert_eq!(undo("changed since\n", &[change]), "changed since\n");
    }

    #[test]
    fn created_file_was_nothing_before() {
        let write = from_tool(
            "Write",
            &json!({}),
            Some(&json!({"type": "create", "filePath": "/p/n", "content": "a\nb\n"})),
        )
        .unwrap();
        let later = edit("/p/n", "b", "c");
        let rows = session_rows("a\nc\n", &[write, later]);
        assert!(rows.iter().all(|r| r.kind == LineKind::Added));
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn crlf_files_compare_by_line() {
        let rows = session_rows("a\r\nB\r\n", &[edit("/p/a", "b", "B")]);
        assert_eq!(kinds(&rows)[0], (LineKind::Context, Some(1), Some(1), "a"));
        assert_eq!(change_starts(&rows), [1]);
    }
}
