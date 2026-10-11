//! File references in Claude's text: `src/main.rs:42`, `greet.py`, `/abs/path/x.rs:3:7`.
//! Each one that names a real project file becomes a link that opens the file at that line.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A file reference found in text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// Byte range in the text.
    pub start: usize,
    pub end: usize,
    /// Absolute path of the file.
    pub path: PathBuf,
    /// 1-based line, when the reference has one.
    pub line: Option<u32>,
}

/// The project's files, for resolving what a reference means.
#[derive(Clone, Debug, Default)]
pub struct FileIndex {
    root: PathBuf,
    /// Relative paths (with `/`).
    relative: std::collections::HashSet<String>,
    /// File name to relative paths with that name.
    by_name: HashMap<String, Vec<String>>,
}

impl FileIndex {
    /// Build from the `(relative path, is_dir)` list a [`crate::host::Host`] gives.
    pub fn new(root: &Path, list: &[(PathBuf, bool)]) -> Self {
        let mut index = Self {
            root: root.to_path_buf(),
            ..Default::default()
        };
        for (rel, is_dir) in list {
            if *is_dir {
                continue;
            }
            let rel = rel.to_string_lossy().replace('\\', "/");
            let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
            index.by_name.entry(name).or_default().push(rel.clone());
            index.relative.insert(rel);
        }
        index
    }

    pub fn len(&self) -> usize {
        self.relative.len()
    }

    pub fn is_empty(&self) -> bool {
        self.relative.is_empty()
    }

    /// The project file a reference names: a relative path, an absolute path inside the
    /// project, or a bare file name that only one file has.
    pub fn resolve(&self, reference: &str) -> Option<PathBuf> {
        let r = reference.trim_start_matches("./");
        if let Some(rel) = Path::new(r)
            .strip_prefix(&self.root)
            .ok()
            .map(|p| p.to_string_lossy().to_string())
        {
            return self.relative.contains(&rel).then(|| self.root.join(rel));
        }
        if self.relative.contains(r) {
            return Some(self.root.join(r));
        }
        if !r.contains('/')
            && let Some([only]) = self.by_name.get(r).map(Vec::as_slice)
        {
            return Some(self.root.join(only));
        }
        None
    }

    /// Every resolvable file reference in `text`.
    pub fn find_links(&self, text: &str) -> Vec<Link> {
        let mut out = Vec::new();
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if !is_path_char(bytes[i]) || (i > 0 && is_path_char(bytes[i - 1])) {
                i += 1;
                continue;
            }
            let mut end = i;
            while end < bytes.len() && is_path_char(bytes[end]) {
                end += 1;
            }
            let token = &text[i..end];
            if let Some(link) = self.token_link(token, i) {
                out.push(link);
            }
            i = end;
        }
        out
    }

    fn token_link(&self, token: &str, offset: usize) -> Option<Link> {
        // Trailing punctuation is part of the sentence, not the path.
        let token = token.trim_end_matches(['.', ',', ':', ')']);
        if !token.contains('.') && !token.contains('/') {
            return None;
        }
        // `path:line` or `path:line:col`.
        let mut parts = token.splitn(3, ':');
        let path = parts.next()?;
        let line = parts
            .next()
            .and_then(|l| l.parse::<u32>().ok())
            .filter(|l| *l > 0);
        let len = match line {
            Some(l) => path.len() + 1 + l.to_string().len(),
            None => path.len(),
        };
        let resolved = self.resolve(path)?;
        Some(Link {
            start: offset,
            end: offset + len,
            path: resolved,
            line,
        })
    }
}

/// The URL scheme of a file link: `workbench-open:<path>#L<line>`.
pub const SCHEME: &str = "workbench-open:";

/// The link URL for a file and line.
pub fn link_url(path: &Path, line: Option<u32>) -> String {
    let mut url = format!("{SCHEME}{}", path.display()).replace(' ', "%20");
    if let Some(l) = line {
        url.push_str(&format!("#L{l}"));
    }
    url
}

/// Read a URL made by [`link_url`] back.
pub fn parse_url(url: &str) -> Option<(PathBuf, Option<u32>)> {
    let rest = url.strip_prefix(SCHEME)?;
    let (path, line) = match rest.rsplit_once("#L") {
        Some((p, l)) => (p, l.parse().ok()),
        None => (rest, None),
    };
    Some((PathBuf::from(path.replace("%20", " ")), line))
}

impl FileIndex {
    /// Turn file references in markdown into links: `` `src/a.rs:3` `` becomes
    /// ``[`src/a.rs:3`](workbench-open:/p/src/a.rs#L3)``. Fenced code blocks and existing links
    /// are left alone.
    pub fn linkify_markdown(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 64);
        let mut in_fence = false;
        for line in text.split_inclusive('\n') {
            if line.trim_start().starts_with("```") {
                in_fence = !in_fence;
                out.push_str(line);
                continue;
            }
            if in_fence || line.contains("](") {
                out.push_str(line);
                continue;
            }
            let mut last = 0;
            for link in self.find_links(line) {
                let (mut start, mut end) = (link.start, link.end);
                // Take the backticks around a code span into the link text.
                let ticked =
                    start > 0 && line.as_bytes()[start - 1] == b'`' && line[end..].starts_with('`');
                if ticked {
                    start -= 1;
                    end += 1;
                } else if line[..start].matches('`').count() % 2 == 1 {
                    // Inside a longer code span: a link would break it.
                    continue;
                }
                if start < last {
                    continue;
                }
                out.push_str(&line[last..start]);
                out.push('[');
                out.push_str(&line[start..end]);
                out.push_str("](");
                out.push_str(&link_url(&link.path, link.line));
                out.push(')');
                last = end;
            }
            out.push_str(&line[last..]);
        }
        out
    }
}

fn is_path_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b':' | b'~' | b'+' | b'@')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> FileIndex {
        let list: Vec<(PathBuf, bool)> = [
            "src/main.rs",
            "src/lib.rs",
            "greet.py",
            "a/mod.rs",
            "b/mod.rs",
            "src",
        ]
        .iter()
        .map(|p| (PathBuf::from(p), *p == "src"))
        .collect();
        FileIndex::new(Path::new("/p"), &list)
    }

    #[test]
    fn finds_paths_with_lines() {
        let text = "See src/main.rs:42 and greet.py, then /p/src/lib.rs:3:7.";
        let links = index().find_links(text);
        assert_eq!(links.len(), 3);
        assert_eq!(&text[links[0].start..links[0].end], "src/main.rs:42");
        assert_eq!(links[0].path, PathBuf::from("/p/src/main.rs"));
        assert_eq!(links[0].line, Some(42));
        assert_eq!(&text[links[1].start..links[1].end], "greet.py");
        assert_eq!(links[1].line, None);
        assert_eq!(links[2].path, PathBuf::from("/p/src/lib.rs"));
        assert_eq!(links[2].line, Some(3));
        assert_eq!(&text[links[2].start..links[2].end], "/p/src/lib.rs:3");
    }

    #[test]
    fn ignores_unknown_and_ambiguous() {
        let idx = index();
        assert!(
            idx.find_links("mod.rs is in two folders, e.g. and nope.rs")
                .is_empty()
        );
        assert_eq!(idx.find_links("a/mod.rs").len(), 1);
        assert!(idx.find_links("version 1.2.3").is_empty());
        assert_eq!(
            idx.resolve("./greet.py"),
            Some(PathBuf::from("/p/greet.py"))
        );
        assert_eq!(idx.resolve("/elsewhere/greet.py"), None);
    }

    #[test]
    fn linkify_markdown_wraps_code_spans_and_skips_fences() {
        let idx = index();
        let md = "Edited `greet.py` and src/main.rs:4.\n```\ngreet.py\n```\nsee [x](y) greet.py\n";
        let out = idx.linkify_markdown(md);
        assert!(
            out.contains("[`greet.py`](workbench-open:/p/greet.py)"),
            "{out}"
        );
        assert!(
            out.contains("[src/main.rs:4](workbench-open:/p/src/main.rs#L4)."),
            "{out}"
        );
        assert!(out.contains("```\ngreet.py\n```"), "{out}");
        assert!(out.ends_with("see [x](y) greet.py\n"), "{out}");
        assert_eq!(
            parse_url("workbench-open:/p/my%20dir/a.rs#L7"),
            Some((PathBuf::from("/p/my dir/a.rs"), Some(7)))
        );
        assert_eq!(
            parse_url(&link_url(Path::new("/p/a"), None)),
            Some((PathBuf::from("/p/a"), None))
        );
        assert_eq!(parse_url("https://x"), None);
    }

    #[test]
    fn markdown_backticks() {
        let links = index().find_links("Changed `greet.py` (line 2)");
        assert_eq!(links.len(), 1);
    }
}
