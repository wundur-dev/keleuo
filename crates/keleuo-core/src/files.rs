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

/// What a file is by its name, for opening it: an image (shown), something only another app
/// opens (its kind, said; not read), or anything else (read: text, or found to be binary).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Other(&'static str),
    Text,
}

pub fn kind_of(path: &Path) -> Kind {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "ico" | "svg" => Kind::Image,
        "pdf" => Kind::Other("PDF document"),
        "mp4" | "mov" | "m4v" | "webm" | "mkv" | "avi" => Kind::Other("Video"),
        "mp3" | "wav" | "m4a" | "aac" | "flac" | "ogg" | "aiff" => Kind::Other("Audio"),
        "zip" | "gz" | "tgz" | "tar" | "bz2" | "xz" | "7z" | "rar" | "zst" => Kind::Other("Archive"),
        "dmg" | "pkg" | "iso" => Kind::Other("Disk image"),
        "doc" | "docx" | "pages" | "rtf" | "odt" => Kind::Other("Document"),
        "xls" | "xlsx" | "numbers" | "ods" => Kind::Other("Spreadsheet"),
        "ppt" | "pptx" | "key" | "odp" => Kind::Other("Presentation"),
        "psd" | "ai" | "sketch" | "fig" | "heic" | "avif" | "raw" | "cr2" | "nef" => Kind::Other("Image"),
        "ttf" | "otf" | "woff" | "woff2" => Kind::Other("Font"),
        "app" | "exe" | "dll" | "so" | "dylib" | "o" | "a" | "wasm" | "class" | "jar" => Kind::Other("Program"),
        "sqlite" | "db" | "sqlite3" => Kind::Other("Database"),
        _ => Kind::Text,
    }
}

/// A size as people read it: 840 bytes, 12.4 KB, 3.1 MB.
pub fn size_text(bytes: u64) -> String {
    let b = bytes as f64;
    match bytes {
        0..1_000 => format!("{bytes} bytes"),
        1_000..1_000_000 => format!("{:.1} KB", b / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1} MB", b / 1e6),
        _ => format!("{:.1} GB", b / 1e9),
    }
}

/// Make a new empty file (or a folder) named `name` in the folder `dir`. The name may hold
/// folders (`src/util/x.rs`), made as needed, but cannot leave `dir`. The error is said to the
/// person as it is.
pub fn create(host: &dyn Host, dir: &Path, name: &str, folder: bool) -> Result<PathBuf, String> {
    let name = name.trim().trim_end_matches('/');
    if name.is_empty() {
        return Err("Type a name.".into());
    }
    let rel = Path::new(name);
    if !rel.components().all(|c| matches!(c, std::path::Component::Normal(_))) {
        return Err("Use a name inside this folder.".into());
    }
    let path = dir.join(rel);
    if host.exists(&path) {
        return Err(format!("{name} already exists."));
    }
    let made = if folder {
        host.create_dir_all(&path)
    } else {
        path.parent().map_or(Ok(()), |p| host.create_dir_all(p)).and_then(|()| host.write(&path, b""))
    };
    made.map(|()| path).map_err(|e| e.to_string())
}

/// Give `path` (a file or folder) the name `name`, in the same folder. The error is said to the
/// person as it is.
pub fn rename(host: &dyn Host, path: &Path, name: &str) -> Result<PathBuf, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Type a name.".into());
    }
    if name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err("A name, not a path (use Move to move it).".into());
    }
    let to = path.with_file_name(name);
    if to == path {
        return Ok(to);
    }
    if host.exists(&to) {
        return Err(format!("{name} already exists."));
    }
    host.rename(path, &to).map(|()| to).map_err(|e| e.to_string())
}

/// Take `path` (a file or folder) away: into the Trash on this machine (true), deleted over
/// SSH, where there is none (false).
pub fn discard(host: &dyn Host, path: &Path) -> Result<bool, String> {
    if host.is_remote() {
        let gone = if host.is_file(path) { host.remove_file(path) } else { host.remove_dir_all(path) };
        gone.map(|()| false).map_err(|e| e.to_string())
    } else {
        trash::delete(path).map(|()| true).map_err(|e| format!("Could not move it to the Trash: {e}"))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn files_are_told_apart_by_name_and_sizes_said_plainly() {
        use std::path::Path;
        assert_eq!(super::kind_of(Path::new("a/Logo.PNG")), super::Kind::Image);
        assert_eq!(super::kind_of(Path::new("icon.svg")), super::Kind::Image);
        assert_eq!(super::kind_of(Path::new("report.pdf")), super::Kind::Other("PDF document"));
        assert_eq!(super::kind_of(Path::new("clip.mov")), super::Kind::Other("Video"));
        assert_eq!(super::kind_of(Path::new("main.rs")), super::Kind::Text);
        assert_eq!(super::kind_of(Path::new("Makefile")), super::Kind::Text);
        assert_eq!(super::size_text(840), "840 bytes");
        assert_eq!(super::size_text(12_400), "12.4 KB");
        assert_eq!(super::size_text(3_100_000), "3.1 MB");
    }

    #[test]
    fn a_file_is_renamed_in_its_folder() {
        use crate::host::LocalHost;
        let dir = std::env::temp_dir().join(format!("keleuo-rename-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        std::fs::write(dir.join("b.txt"), "y").unwrap();
        assert_eq!(super::rename(&LocalHost, &dir.join("a.txt"), "c.txt"), Ok(dir.join("c.txt")));
        assert!(dir.join("c.txt").is_file() && !dir.join("a.txt").exists());
        assert!(super::rename(&LocalHost, &dir.join("c.txt"), "b.txt").is_err(), "taken");
        assert!(super::rename(&LocalHost, &dir.join("c.txt"), "sub/d.txt").is_err(), "a path");
        let _ = std::fs::remove_dir_all(&dir);
    }

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

    #[test]
    fn creates_files_and_folders_inside_the_folder_only() {
        let host = crate::host::LocalHost;
        let dir = std::env::temp_dir().join(format!("keleuo-create-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = create(&host, &dir, " notes.md ", false).unwrap();
        assert!(file.is_file() && file.ends_with("notes.md"));
        let nested = create(&host, &dir, "src/util/x.rs", false).unwrap();
        assert!(nested.is_file());
        assert!(create(&host, &dir, "assets/icons/", true).unwrap().is_dir());
        assert_eq!(create(&host, &dir, "notes.md", false), Err("notes.md already exists.".into()));
        assert!(create(&host, &dir, "../out.txt", false).is_err());
        assert!(create(&host, &dir, "/etc/x", false).is_err());
        assert!(create(&host, &dir, "  ", true).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
