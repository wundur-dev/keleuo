//! The project file tree, respecting .gitignore (and .ignore), with dot files shown but `.git`
//! left out.

use std::path::{Path, PathBuf};

use crate::host::Host;

/// Files listed at most, so a huge folder does not stall the window.
pub const MAX_ENTRIES: usize = 20_000;

/// A file or folder of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    /// Absolute path.
    pub path: PathBuf,
    /// `Some` for a folder (sorted: folders first, then files, by name).
    pub children: Option<Vec<Node>>,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.children.is_some()
    }
}

/// The tree of `root`: its children, folders first.
pub fn tree(host: &dyn Host, root: &Path) -> Vec<Node> {
    tree_from_list(root, host.list_files(root, MAX_ENTRIES))
}

/// Build the tree from a flat `(relative path, is_dir)` list.
pub fn tree_from_list(root: &Path, list: Vec<(PathBuf, bool)>) -> Vec<Node> {
    let mut top: Vec<Node> = Vec::new();
    for (rel, is_dir) in list {
        insert(&mut top, root, &rel, is_dir);
    }
    sort(&mut top);
    top
}

fn insert(level: &mut Vec<Node>, root: &Path, rel: &Path, is_dir: bool) {
    let mut parts = rel.components().peekable();
    let mut level = level;
    let mut path = root.to_path_buf();
    while let Some(part) = parts.next() {
        let name = part.as_os_str().to_string_lossy().to_string();
        path.push(&name);
        let last = parts.peek().is_none();
        let index = match level.iter().position(|n| n.name == name) {
            Some(i) => i,
            None => {
                level.push(Node {
                    name,
                    path: path.clone(),
                    children: (!last || is_dir).then(Vec::new),
                });
                level.len() - 1
            }
        };
        match level[index].children.as_mut() {
            Some(children) if !last => level = children,
            _ => return,
        }
    }
}

fn sort(level: &mut [Node]) {
    level.sort_by(|a, b| {
        b.is_dir()
            .cmp(&a.is_dir())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    for node in level {
        if let Some(children) = node.children.as_mut() {
            sort(children);
        }
    }
}

/// A file is shown as text when it has no NUL byte in its first 8 KB.
pub fn looks_like_text(bytes: &[u8]) -> bool {
    !bytes[..bytes.len().min(8192)].contains(&0)
}

/// The language name for highlighting, from the file name.
pub fn language_for(path: &Path) -> &'static str {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match name.as_str() {
        "makefile" | "gnumakefile" => return "make",
        "cmakelists.txt" => return "cmake",
        "dockerfile" => return "bash",
        _ => {}
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "json" | "jsonc" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "css" | "scss" => "css",
        "sh" | "bash" | "zsh" => "bash",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => "cpp",
        "cs" => "csharp",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "rb" => "ruby",
        "php" => "php",
        "lua" => "lua",
        "sql" => "sql",
        "zig" => "zig",
        "scala" => "scala",
        "ex" | "exs" => "elixir",
        "proto" => "proto",
        "graphql" | "gql" => "graphql",
        "diff" | "patch" => "diff",
        "svelte" => "svelte",
        "astro" => "astro",
        _ => "text",
    }
}

pub fn is_markdown(path: &Path) -> bool {
    language_for(path) == "markdown"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respects_gitignore_and_sorts_folders_first() {
        let root = std::env::temp_dir().join(format!("wb-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for p in [
            "src/main.rs",
            "src/lib.rs",
            "target/debug/x",
            ".git/HEAD",
            "README.md",
            ".env.example",
            "a.log",
        ] {
            let path = root.join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        std::fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();

        let names: Vec<String> = tree(&crate::host::LocalHost, &root)
            .iter()
            .map(|n| n.name.clone())
            .collect();
        assert_eq!(names, ["src", ".env.example", ".gitignore", "README.md"]);
        let src = &tree(&crate::host::LocalHost, &root)[0];
        let kids: Vec<&str> = src
            .children
            .as_ref()
            .unwrap()
            .iter()
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(kids, ["lib.rs", "main.rs"]);
        assert_eq!(
            src.children.as_ref().unwrap()[0].path,
            root.join("src/lib.rs")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn languages() {
        assert_eq!(language_for(Path::new("a/b.rs")), "rust");
        assert_eq!(language_for(Path::new("Makefile")), "make");
        assert_eq!(language_for(Path::new("x.unknown")), "text");
        assert!(is_markdown(Path::new("README.MD")));
        assert!(looks_like_text(b"hello"));
        assert!(!looks_like_text(b"he\0llo"));
    }
}
