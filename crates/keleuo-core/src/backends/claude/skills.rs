//! Skills and other Claude Code customisations relevant to one project.
//!
//! Where Claude Code keeps them:
//! - project skills: `<project>/.claude/skills/<name>/SKILL.md`;
//! - user skills: `~/.claude/skills/<name>/SKILL.md`;
//! - plugin skills: `<installPath>/skills/<name>/SKILL.md` for each plugin listed in
//!   `~/.claude/plugins/installed_plugins.json` (user scope, or project scope for this project);
//! - project agents `<project>/.claude/agents/*.md` and commands `<project>/.claude/commands/*.md`;
//! - `CLAUDE.md` at the project root and in `.claude/`.

use std::path::{Path, PathBuf};

use crate::frontmatter;
use crate::host::Host;

/// Where an entry comes from.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Project,
    User,
    /// An installed plugin, by its name (`superpowers`, not `superpowers@marketplace`).
    Plugin(String),
}

/// What kind of file an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Skill,
    Agent,
    Command,
    ClaudeMd,
}

/// One skill, agent, command or CLAUDE.md file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub source: Source,
    /// The frontmatter name, or the folder or file name when there is none.
    pub name: String,
    pub description: String,
    /// The file itself (SKILL.md for a skill).
    pub path: PathBuf,
}

impl Entry {
    /// Plugin files belong to the plugin cache and are overwritten on update.
    pub fn read_only(&self) -> bool {
        matches!(self.source, Source::Plugin(_))
    }
}

/// Everything found for a project, grouped for the side panel.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub project: Vec<Entry>,
    pub user: Vec<Entry>,
    /// `(plugin name, its skills)`, sorted by plugin name.
    pub plugins: Vec<(String, Vec<Entry>)>,
    /// CLAUDE.md files, agents and commands of the project.
    pub project_docs: Vec<Entry>,
}

impl Catalog {
    /// The neutral groups the window shows.
    pub fn into_neutral(self) -> crate::skills::Catalog {
        let conv = |e: Entry| crate::skills::Entry {
            kind: match e.kind {
                Kind::Skill => crate::skills::Kind::Skill,
                Kind::Agent => crate::skills::Kind::Agent,
                Kind::Command => crate::skills::Kind::Command,
                Kind::ClaudeMd => crate::skills::Kind::Instructions,
            },
            read_only: e.read_only(),
            name: e.name,
            description: e.description,
            path: e.path,
        };
        let group = |key: &str, title: String, entries: Vec<Entry>, folded: bool| crate::skills::Group {
            key: key.to_string(),
            title,
            entries: entries.into_iter().map(conv).collect(),
            folded,
        };
        let mut groups = vec![group("project", "Project".into(), self.project, false)];
        if !self.project_docs.is_empty() {
            groups.push(group("docs", "Instructions, agents, commands".into(), self.project_docs, false));
        }
        groups.push(group("user", "User".into(), self.user, false));
        for (name, entries) in self.plugins {
            groups.push(group(&format!("plugin:{name}"), format!("Plugin: {name}"), entries, true));
        }
        crate::skills::Catalog { groups }
    }

    pub fn all(&self) -> impl Iterator<Item = &Entry> {
        self.project
            .iter()
            .chain(&self.user)
            .chain(self.plugins.iter().flat_map(|(_, s)| s))
            .chain(&self.project_docs)
    }

    pub fn skill_count(&self) -> usize {
        self.project.len()
            + self.user.len()
            + self.plugins.iter().map(|(_, s)| s.len()).sum::<usize>()
    }
}

/// Claude Code's `instructionFiles` setting (which instruction files it loads): the project's
/// local settings, then its shared ones, then the user's; `claude-md-or-agents-md` (its
/// default) when none says.
fn instruction_files(host: &dyn Host, project: &Path, claude_home: Option<&Path>) -> String {
    let mut files = vec![project.join(".claude/settings.local.json"), project.join(".claude/settings.json")];
    if let Some(home) = claude_home {
        files.push(home.join("settings.json"));
    }
    files
        .iter()
        .filter_map(|f| host.read_to_string(f).ok())
        .filter_map(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .find_map(|v| v.get("instructionFiles").and_then(|m| m.as_str()).map(str::to_string))
        .unwrap_or_else(|| "claude-md-or-agents-md".into())
}

/// Scan everything for `project`, with `claude_home` the user's `~/.claude`.
pub fn discover(host: &dyn Host, project: &Path, claude_home: Option<&Path>) -> Catalog {
    let mut catalog = Catalog {
        project: skills_in(host, &project.join(".claude/skills"), Source::Project),
        ..Default::default()
    };
    if let Some(home) = claude_home {
        catalog.user = skills_in(host, &home.join("skills"), Source::User);
        catalog.plugins = plugin_skills(host, home, project);
    }
    let mut imports = String::new();
    for name in ["CLAUDE.md", ".claude/CLAUDE.md", "CLAUDE.local.md"] {
        let path = project.join(name);
        if host.is_file(&path) {
            imports.push_str(&host.read_to_string(&path).unwrap_or_default());
            catalog.project_docs.push(Entry {
                kind: Kind::ClaudeMd,
                source: Source::Project,
                name: name.to_string(),
                description: "Project instructions Claude reads every session".into(),
                path,
            });
        }
    }
    // AGENTS.md, when Claude reads it: imported by a CLAUDE.md (`@AGENTS.md`), or as the
    // project's instructions (`instructionFiles`: in place of a missing CLAUDE.md by default,
    // beside it when set so).
    let has_claude_md = !catalog.project_docs.is_empty();
    let mode = instruction_files(host, project, claude_home);
    for name in ["AGENTS.md", ".claude/AGENTS.md"] {
        let path = project.join(name);
        if !host.is_file(&path) {
            continue;
        }
        let imported = imports.lines().any(|l| {
            let l = l.trim();
            l.strip_prefix('@').is_some_and(|i| i.trim_start_matches("./") == name)
        });
        let description = if imported {
            "Imported by CLAUDE.md"
        } else {
            match mode.as_str() {
                "claude-md-and-agents-md" => "Read beside CLAUDE.md",
                "claude-md" | "managed-only" => continue,
                _ if has_claude_md => continue,
                _ => "Read as the project's instructions (there is no CLAUDE.md)",
            }
        };
        catalog.project_docs.push(Entry {
            kind: Kind::ClaudeMd,
            source: Source::Project,
            name: name.to_string(),
            description: description.into(),
            path,
        });
    }
    catalog.project_docs.extend(markdown_files_in(
        host,
        &project.join(".claude/agents"),
        Kind::Agent,
    ));
    catalog.project_docs.extend(markdown_files_in(
        host,
        &project.join(".claude/commands"),
        Kind::Command,
    ));
    catalog
}

/// The `~/.claude` folder of the host.
pub fn claude_home(host: &dyn Host) -> Option<PathBuf> {
    host.home_dir().map(|h| h.join(".claude"))
}

/// Skills in a `skills` folder: each subfolder holding a SKILL.md.
pub fn skills_in(host: &dyn Host, dir: &Path, source: Source) -> Vec<Entry> {
    let Ok(read) = host.read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Entry> = read
        .into_iter()
        .filter(|e| e.is_dir)
        .filter_map(|e| {
            let path = dir.join(&e.name).join("SKILL.md");
            let text = host.read_to_string(&path).ok()?;
            Some(entry_from_text(
                Kind::Skill,
                source.clone(),
                &path,
                &text,
                &e.name,
            ))
        })
        .collect();
    out.sort_by_key(|e| e.name.to_lowercase());
    out
}

fn markdown_files_in(host: &dyn Host, dir: &Path, kind: Kind) -> Vec<Entry> {
    let Ok(read) = host.read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Entry> = read
        .into_iter()
        .filter(|e| !e.is_dir && e.name.ends_with(".md"))
        .filter_map(|e| {
            let path = dir.join(&e.name);
            let text = host.read_to_string(&path).ok()?;
            let stem = e.name.trim_end_matches(".md").to_string();
            Some(entry_from_text(kind, Source::Project, &path, &text, &stem))
        })
        .collect();
    out.sort_by_key(|e| e.name.to_lowercase());
    out
}

fn entry_from_text(kind: Kind, source: Source, path: &Path, text: &str, fallback: &str) -> Entry {
    let fm = frontmatter::parse(text);
    Entry {
        kind,
        source,
        name: fm.name.unwrap_or_else(|| fallback.to_string()),
        description: fm.description.unwrap_or_default(),
        path: path.to_path_buf(),
    }
}

/// Skills of installed plugins, from `installed_plugins.json`.
fn plugin_skills(host: &dyn Host, claude_home: &Path, project: &Path) -> Vec<(String, Vec<Entry>)> {
    let file = claude_home.join("plugins/installed_plugins.json");
    let Ok(text) = host.read_to_string(&file) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for plugin in installed_plugins(&text, project) {
        let skills = skills_in(
            host,
            &plugin.install_path.join("skills"),
            Source::Plugin(plugin.name.clone()),
        );
        if !skills.is_empty() {
            out.push((plugin.name, skills));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// An installed plugin that applies to the project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledPlugin {
    pub name: String,
    pub install_path: PathBuf,
}

/// Parse `installed_plugins.json` (version 2: `{"plugins": {"name@market": [install, ...]}}`),
/// keeping user-scope installs and project or local installs whose `projectPath` is `project`.
pub fn installed_plugins(json: &str, project: &Path) -> Vec<InstalledPlugin> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(plugins) = value.get("plugins").and_then(|p| p.as_object()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, installs) in plugins {
        let name = key.split('@').next().unwrap_or(key).to_string();
        for install in installs.as_array().into_iter().flatten() {
            let scope = install
                .get("scope")
                .and_then(|s| s.as_str())
                .unwrap_or("user");
            let applies = match scope {
                "user" | "managed" => true,
                _ => install
                    .get("projectPath")
                    .and_then(|p| p.as_str())
                    .is_some_and(|p| Path::new(p) == project),
            };
            if let (true, Some(path)) =
                (applies, install.get("installPath").and_then(|p| p.as_str()))
            {
                out.push(InstalledPlugin {
                    name: name.clone(),
                    install_path: PathBuf::from(path),
                });
                break;
            }
        }
    }
    out
}

/// Turn free text into a skill folder name: lower case, words joined by `-`.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// The SKILL.md a new project skill starts from.
pub fn skill_template(name: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: Use when ... (say when Claude should load this skill)\n---\n\n\
         # {name}\n\nWhat this skill is for, in a sentence or two.\n\n## Steps\n\n1. First step.\n2. Second step.\n"
    )
}

/// Create `<project>/.claude/skills/<slug>/SKILL.md` from the template. A number is added to the
/// name when the folder already exists. Returns the new file.
pub fn create_project_skill(
    host: &dyn Host,
    project: &Path,
    name: &str,
) -> std::io::Result<PathBuf> {
    create_skill_in(host, &project.join(".claude/skills"), name)
}

/// Create skill `name` (as a folder name) in the skills folder `skills`, from the template;
/// returns its SKILL.md.
pub fn create_skill_in(host: &dyn Host, skills: &Path, name: &str) -> std::io::Result<PathBuf> {
    let base = match slug(name) {
        s if s.is_empty() => "new-skill".to_string(),
        s => s,
    };
    let mut folder = base.clone();
    let mut n = 2;
    while host.exists(&skills.join(&folder)) {
        folder = format!("{base}-{n}");
        n += 1;
    }
    let dir = skills.join(&folder);
    host.create_dir_all(&dir)?;
    let path = dir.join("SKILL.md");
    host.write(&path, skill_template(&folder).as_bytes())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::LocalHost;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wb-skills-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn discovers_all_three_sources() {
        let root = temp("all");
        let project = root.join("proj");
        let home = root.join("home/.claude");
        write(
            &project.join(".claude/skills/deploy/SKILL.md"),
            "---\nname: deploy\ndescription: Ship it\n---\nbody",
        );
        write(
            &project.join(".claude/skills/not-a-skill/README.md"),
            "ignored",
        );
        write(&project.join("CLAUDE.md"), "# Rules");
        write(
            &project.join(".claude/agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Reviews\n---\n",
        );
        write(&project.join(".claude/commands/ship.md"), "Ship the thing");
        write(
            &home.join("skills/zeta/SKILL.md"),
            "---\nname: zeta\ndescription: Last\n---\n",
        );
        write(&home.join("skills/Alpha/SKILL.md"), "no frontmatter");
        let plugin = root.join("cache/sp/1.0");
        write(
            &plugin.join("skills/tdd/SKILL.md"),
            "---\nname: tdd\ndescription: Tests first\n---\n",
        );
        let other = root.join("cache/other/1.0");
        write(&other.join("skills/x/SKILL.md"), "---\nname: x\n---\n");
        let json = serde_json::json!({
            "version": 2,
            "plugins": {
                "superpowers@official": [{"scope": "user", "installPath": plugin}],
                "other@official": [{"scope": "project", "projectPath": "/elsewhere", "installPath": other}],
            }
        });
        write(
            &home.join("plugins/installed_plugins.json"),
            &json.to_string(),
        );

        let catalog = discover(&LocalHost, &project, Some(&home));
        assert_eq!(catalog.project.len(), 1);
        assert_eq!(catalog.project[0].name, "deploy");
        assert_eq!(catalog.project[0].description, "Ship it");
        assert!(!catalog.project[0].read_only());
        let user: Vec<_> = catalog.user.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(user, ["Alpha", "zeta"]);
        assert_eq!(
            catalog.plugins.len(),
            1,
            "the project-scoped plugin of another project is skipped"
        );
        assert_eq!(catalog.plugins[0].0, "superpowers");
        assert_eq!(catalog.plugins[0].1[0].name, "tdd");
        assert!(catalog.plugins[0].1[0].read_only());
        let docs: Vec<_> = catalog
            .project_docs
            .iter()
            .map(|e| (e.kind, e.name.as_str()))
            .collect();
        assert_eq!(
            docs,
            [
                (Kind::ClaudeMd, "CLAUDE.md"),
                (Kind::Agent, "reviewer"),
                (Kind::Command, "ship")
            ]
        );
        assert_eq!(catalog.skill_count(), 4);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn agents_md_is_listed_when_claude_reads_it() {
        let root = temp("agents-md");
        let project = root.join("proj");
        let names = |c: &Catalog| c.project_docs.iter().map(|e| (e.name.clone(), e.description.clone())).collect::<Vec<_>>();
        write(&project.join("AGENTS.md"), "# Shared rules");
        // No CLAUDE.md: Claude reads AGENTS.md in its place.
        let c = discover(&LocalHost, &project, None);
        assert_eq!(names(&c), [("AGENTS.md".into(), "Read as the project's instructions (there is no CLAUDE.md)".into())]);
        // A CLAUDE.md of its own, not importing it: AGENTS.md is not read.
        write(&project.join("CLAUDE.md"), "# Rules");
        assert_eq!(discover(&LocalHost, &project, None).project_docs.len(), 1);
        // Imported: listed as such.
        write(&project.join("CLAUDE.md"), "@AGENTS.md\n\n- more");
        let c = discover(&LocalHost, &project, None);
        assert_eq!(names(&c)[1], ("AGENTS.md".into(), "Imported by CLAUDE.md".into()));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn project_scoped_plugin_for_this_project() {
        let json =
            r#"{"plugins":{"a@m":[{"scope":"local","projectPath":"/p","installPath":"/c/a"}]}}"#;
        let found = installed_plugins(json, Path::new("/p"));
        assert_eq!(
            found,
            [InstalledPlugin {
                name: "a".into(),
                install_path: "/c/a".into()
            }]
        );
        assert!(installed_plugins(json, Path::new("/q")).is_empty());
        assert!(installed_plugins("not json", Path::new("/p")).is_empty());
    }

    #[test]
    fn creates_a_skill_from_the_template() {
        let project = temp("create");
        let path = create_project_skill(&LocalHost, &project, "My New Skill!").unwrap();
        assert!(path.ends_with(".claude/skills/my-new-skill/SKILL.md"));
        let again = create_project_skill(&LocalHost, &project, "my new skill").unwrap();
        assert!(again.ends_with(".claude/skills/my-new-skill-2/SKILL.md"));
        let catalog = discover(&LocalHost, &project, None);
        assert_eq!(catalog.project.len(), 2);
        assert_eq!(catalog.project[0].name, "my-new-skill");
        assert!(catalog.project[0].description.starts_with("Use when"));
        let _ = std::fs::remove_dir_all(project);
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("  Hello, World  "), "hello-world");
        assert_eq!(slug("!!!"), "");
    }
}
