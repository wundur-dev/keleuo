//! Skills and similar files an agent reads (instructions, subagent definitions, commands), in
//! neutral terms: groups of entries for the sidebar. Each backend says where its own live.

use std::path::{Path, PathBuf};

/// What kind of file an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Skill,
    /// A subagent definition.
    Agent,
    Command,
    /// Standing instructions for the project (CLAUDE.md, AGENTS.md, ...).
    Instructions,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    /// Owned by something else (an installed plugin): view only.
    pub read_only: bool,
}

/// A titled group of entries ("Project", "User", "Plugin: x").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// Stable, for remembering whether it is folded.
    pub key: String,
    pub title: String,
    pub entries: Vec<Entry>,
    /// Shown folded at first (long lists).
    pub folded: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Catalog {
    pub groups: Vec<Group>,
}

impl Catalog {
    pub fn all(&self) -> impl Iterator<Item = &Entry> {
        self.groups.iter().flat_map(|g| &g.entries)
    }

    pub fn find(&self, path: &Path) -> Option<&Entry> {
        self.all().find(|e| e.path == path)
    }

    pub fn skill_count(&self) -> usize {
        self.all().filter(|e| e.kind == Kind::Skill).count()
    }
}
