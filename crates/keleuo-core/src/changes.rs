//! What a turn (or a whole session) changed, file by file: the overview behind the Changes
//! panel. Each file has its line counts, whether it is new, and a group (code, tests, or
//! generated files such as lock files, which go last and folded).

use std::path::{Path, PathBuf};

/// How a changed file is listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Code,
    Tests,
    /// Lock files, snapshots, build output and minified files: often half the lines, rarely
    /// what needs reading.
    Generated,
}

/// One changed file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    pub path: PathBuf,
    pub added: usize,
    pub removed: usize,
    /// It did not exist before.
    pub created: bool,
    /// How many edits made the change (none when a command made it).
    pub edits: usize,
    pub group: Group,
    /// Changed by a command, not the agent's edit tools (see [`crate::command_changes`]).
    pub by_command: bool,
}

/// Which group `path` belongs in, from its name alone.
pub fn group_of(path: &Path) -> Group {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let parts: Vec<String> = path.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect();
    let in_dir = |dirs: &[&str]| parts.iter().rev().skip(1).any(|p| dirs.contains(&p.as_str()));
    let generated_names = [
        "cargo.lock",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "bun.lockb",
        "bun.lock",
        "poetry.lock",
        "uv.lock",
        "gemfile.lock",
        "composer.lock",
        "go.sum",
    ];
    if generated_names.contains(&name.as_str())
        || name.ends_with(".lock")
        || name.ends_with(".snap")
        || name.ends_with(".min.js")
        || name.ends_with(".min.css")
        || name.ends_with(".map")
        || in_dir(&["dist", "build", "generated", "__generated__", "node_modules", "target", "__snapshots__"])
    {
        return Group::Generated;
    }
    let stem = name.split('.').next().unwrap_or("");
    if in_dir(&["tests", "test", "__tests__", "spec", "specs"])
        || stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with("_tests")
        || stem == "tests"
        || name.contains(".test.")
        || name.contains(".spec.")
        || (name.ends_with("test.java") || name.ends_with("tests.cs") || name.ends_with("test.kt"))
    {
        return Group::Tests;
    }
    Group::Code
}

/// The changed files, grouped (code, tests, generated) and otherwise in the order they were
/// first changed.
pub fn summarize(diffs: impl IntoIterator<Item = crate::diff::FileDiff>) -> Vec<FileChange> {
    let mut out: Vec<FileChange> = Vec::new();
    for d in diffs {
        let (added, removed) = (d.added(), d.removed());
        match out.iter_mut().find(|c| c.path == d.path) {
            Some(c) => {
                c.added += added;
                c.removed += removed;
                c.edits += 1;
            }
            None => out.push(FileChange {
                group: group_of(&d.path),
                created: d.created,
                path: d.path,
                added,
                removed,
                edits: 1,
                by_command: false,
            }),
        }
    }
    // Stable: the order of first change stays within each group.
    out.sort_by_key(|c| c.group);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_grouped_by_name() {
        for (path, group) in [
            ("/p/src/auth.rs", Group::Code),
            ("/p/src/login.tsx", Group::Code),
            ("/p/Cargo.lock", Group::Generated),
            ("/p/web/package-lock.json", Group::Generated),
            ("/p/web/dist/app.js", Group::Generated),
            ("/p/src/__snapshots__/a.snap", Group::Generated),
            ("/p/tests/login.rs", Group::Tests),
            ("/p/src/auth_test.go", Group::Tests),
            ("/p/src/login.test.ts", Group::Tests),
            ("/p/app/test_models.py", Group::Tests),
            ("/p/src/latest.rs", Group::Code),
            ("/p/src/contest.py", Group::Code),
        ] {
            assert_eq!(group_of(Path::new(path)), group, "{path}");
        }
    }
}
