//! The skills Cursor Agent reads, where it reads them (from cursor-agent 2026.10.14's own
//! list): `<folder>/SKILL.md` under `.cursor/skills`, then the other agents' folders it also
//! reads (`.claude/skills`, `.codex/skills`, `.grok/skills`, `.agents/skills`), in the project
//! and in the home folder; its built-in ones in `~/.cursor/skills-cursor`; and the skills of
//! installed Claude Code plugins. A name found twice is the first one, as Cursor takes it.

use std::path::{Path, PathBuf};

use crate::backends::claude::skills::{self as claude_skills, Source};
use crate::host::Host;
use crate::skills::{Catalog, Entry, Group, Kind};

/// The folders that hold skill folders, in the order Cursor looks.
const FOLDERS: [&str; 5] = [".cursor/skills", ".claude/skills", ".codex/skills", ".grok/skills", ".agents/skills"];

/// Every skill Cursor would load for `project`, grouped for the sidebar.
pub fn discover(host: &dyn Host, project: &Path) -> Catalog {
    let home = host.home_dir();
    let mut seen: Vec<String> = Vec::new();
    let mut take = |dirs: Vec<PathBuf>, read_only: bool| -> Vec<Entry> {
        let mut out = Vec::new();
        for dir in dirs {
            for e in claude_skills::skills_in(host, &dir, Source::User) {
                if seen.contains(&e.name) {
                    continue;
                }
                seen.push(e.name.clone());
                out.push(Entry { kind: Kind::Skill, name: e.name, description: e.description, path: e.path, read_only });
            }
        }
        out
    };
    let project_skills = take(FOLDERS.iter().map(|f| project.join(f)).collect(), false);
    let user_skills = match &home {
        Some(h) => take(FOLDERS.iter().map(|f| h.join(f)).collect(), false),
        None => Vec::new(),
    };
    let builtin = match &home {
        Some(h) => take(vec![h.join(".cursor/skills-cursor")], true),
        None => Vec::new(),
    };
    let mut groups = vec![
        Group { key: "project".into(), title: "Project".into(), entries: project_skills, folded: false },
        Group { key: "user".into(), title: "User".into(), entries: user_skills, folded: false },
    ];
    if !builtin.is_empty() {
        groups.push(Group { key: "cursor".into(), title: "Built into Cursor".into(), entries: builtin, folded: true });
    }
    // Claude Code plugins' skills, which Cursor loads too.
    let plugins = claude_skills::discover(host, project, claude_skills::claude_home(host).as_deref()).into_neutral();
    for g in plugins.groups.into_iter().filter(|g| g.key.starts_with("plugin:")) {
        let entries: Vec<Entry> = g.entries.into_iter().filter(|e| e.kind == Kind::Skill && !seen.contains(&e.name)).collect();
        if !entries.is_empty() {
            groups.push(Group { entries, ..g });
        }
    }
    Catalog { groups }
}

/// A new project skill, in `.cursor/skills`.
pub fn create(host: &dyn Host, project: &Path, name: &str) -> std::io::Result<PathBuf> {
    claude_skills::create_skill_in(host, &project.join(".cursor/skills"), name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::LocalHost;

    #[test]
    fn skills_are_found_where_cursor_looks_first_one_wins() {
        let project = std::env::temp_dir().join(format!("keleuo-cursor-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&project);
        let write = |rel: &str, name: &str| {
            let dir = project.join(rel).join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), format!("---\nname: {name}\ndescription: from {rel}\n---\nbody\n")).unwrap();
        };
        write(".cursor/skills", "review");
        write(".claude/skills", "review");
        write(".agents/skills", "deploy");
        let catalog = discover(&LocalHost, &project);
        let project_group = catalog.groups.iter().find(|g| g.key == "project").unwrap();
        let found: Vec<(&str, &str)> = project_group.entries.iter().map(|e| (e.name.as_str(), e.description.as_str())).collect();
        assert!(found.contains(&("review", "from .cursor/skills")), "{found:?}");
        assert!(found.contains(&("deploy", "from .agents/skills")));
        assert_eq!(found.iter().filter(|(n, _)| *n == "review").count(), 1);
        let made = create(&LocalHost, &project, "New thing").unwrap();
        assert!(made.starts_with(project.join(".cursor/skills")));
        let _ = std::fs::remove_dir_all(&project);
    }
}
