//! Files a turn changed outside the agent's edit tools (a formatter, `sed`, a code generator),
//! found with git: a baseline of the working tree when the turn starts, compared with the
//! working tree when it ends. Only in a git repository on this machine.
//!
//! The baseline is `git stash create` (a commit of the working tree that touches nothing, not
//! even the stash list), or HEAD when nothing is changed. Untracked files are listed at the
//! start: one that appears later is new, but a change to one that was already there is not
//! seen (git has no copy of it).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::changes::{FileChange, group_of};

/// The working tree when a turn started.
#[derive(Clone, Debug)]
pub struct Baseline {
    /// The repository's top folder.
    root: PathBuf,
    /// A commit holding the tracked files as they were.
    commit: String,
    /// Untracked (not ignored) files then, relative to `root`.
    untracked: HashSet<PathBuf>,
}

fn git(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        // Never hold the index lock: the agent may be running git itself.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).trim().to_string()
}

fn untracked(root: &Path) -> Option<HashSet<PathBuf>> {
    let out = git(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    Some(out.split(|b| *b == 0).filter(|p| !p.is_empty()).map(|p| PathBuf::from(String::from_utf8_lossy(p).as_ref())).collect())
}

/// The working tree in `cwd` now, or `None` outside a git repository (or one with no commit).
pub fn baseline(cwd: &Path) -> Option<Baseline> {
    let root = PathBuf::from(text(git(cwd, &["rev-parse", "--show-toplevel"])?));
    let stash = text(git(&root, &["stash", "create"])?);
    let commit = if stash.is_empty() { text(git(&root, &["rev-parse", "HEAD"])?) } else { stash };
    Some(Baseline { untracked: untracked(&root)?, root, commit })
}

/// What changed in the working tree since `base`, leaving out `skip` (the files the agent's
/// edit tools changed, already counted). Each is marked as changed by a command.
pub fn since(base: &Baseline, skip: &HashSet<PathBuf>) -> Vec<FileChange> {
    let mut out = Vec::new();
    let Some(numstat) = git(&base.root, &["diff", "--numstat", "--no-renames", "-z", &base.commit]) else {
        return out;
    };
    let added: HashSet<PathBuf> = git(&base.root, &["diff", "--name-only", "--diff-filter=A", "--no-renames", "-z", &base.commit])
        .map(|o| o.split(|b| *b == 0).filter(|p| !p.is_empty()).map(|p| PathBuf::from(String::from_utf8_lossy(p).as_ref())).collect())
        .unwrap_or_default();
    for entry in numstat.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let entry = String::from_utf8_lossy(entry);
        let mut parts = entry.splitn(3, '\t');
        let (Some(a), Some(r), Some(rel)) = (parts.next(), parts.next(), parts.next()) else { continue };
        // Binary files count as no lines ("-").
        let (a, r) = (a.parse().unwrap_or(0), r.parse().unwrap_or(0));
        let rel = PathBuf::from(rel);
        out.push(change(base.root.join(&rel), a, r, added.contains(&rel)));
    }
    for rel in untracked(&base.root).unwrap_or_default() {
        if base.untracked.contains(&rel) {
            continue;
        }
        let path = base.root.join(&rel);
        let lines = std::fs::read(&path).map(|b| if b.contains(&0) { 0 } else { b.iter().filter(|c| **c == b'\n').count() }).unwrap_or(0);
        out.push(change(path, lines, 0, true));
    }
    out.retain(|c| !skip.contains(&c.path));
    out.sort_by(|a, b| a.group.cmp(&b.group).then_with(|| a.path.cmp(&b.path)));
    out
}

fn change(path: PathBuf, added: usize, removed: usize, created: bool) -> FileChange {
    FileChange { group: group_of(&path), path, added, removed, created, edits: 0, by_command: true }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, script: &str) {
        let ok = Command::new("sh").arg("-c").arg(script).current_dir(dir).status().unwrap().success();
        assert!(ok, "{script}");
    }

    #[test]
    fn a_command_changes_and_new_files_since_the_baseline() {
        let dir = std::env::temp_dir().join(format!("keleuo-cmd-changes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        sh(
            &dir,
            "git init -q && git config user.email t@t && git config user.name t && \
             printf 'a\\nb\\n' > kept.rs && printf 'x\\n' > edited.rs && printf 'old\\n' > scratch.txt && \
             git add kept.rs edited.rs && git commit -qm one && printf 'dirty\\n' >> kept.rs",
        );
        let root = PathBuf::from(text(git(&dir, &["rev-parse", "--show-toplevel"]).unwrap()));
        let base = baseline(&dir).unwrap();
        // The stash list is untouched.
        assert_eq!(text(git(&dir, &["stash", "list"]).unwrap()), "");
        // During the turn: a command rewrites kept.rs, the edit tool changes edited.rs, a
        // generator writes a new file, and the agent commits.
        sh(&dir, "printf 'a\\nB\\nc\\n' > kept.rs && printf 'y\\n' > edited.rs && printf '1\\n2\\n' > gen.rs && git commit -qam two");
        let skip: HashSet<PathBuf> = [root.join("edited.rs")].into();
        let found: Vec<_> = since(&base, &skip).into_iter().map(|c| (c.path, c.added, c.removed, c.created)).collect();
        assert_eq!(found, [(root.join("gen.rs"), 2, 0, true), (root.join("kept.rs"), 2, 2, false)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
