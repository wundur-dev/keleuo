//! What a message starting with `/` can be: the agent's own commands, and the skills and
//! commands it finds (a plugin's as `plugin:name`). The message box lists the ones matching
//! what is typed after the `/`.

use crate::skills::{Catalog, Kind};

/// One thing a `/` can name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    /// Without the `/`.
    pub name: String,
    pub description: String,
    /// Where it comes from: "Skill", "Command", "Plugin: x", or the agent's name.
    pub from: String,
}

/// The skills and commands of `catalog`, then the agent's own (`builtin`, with `agent` naming it).
pub fn choices(catalog: &Catalog, builtin: &[(&str, &str)], agent: &str) -> Vec<Choice> {
    let mut out: Vec<Choice> = Vec::new();
    for group in &catalog.groups {
        let plugin = group.key.strip_prefix("plugin:");
        for e in group.entries.iter().filter(|e| matches!(e.kind, Kind::Skill | Kind::Command)) {
            let name = e.name.trim_end_matches(".md");
            let name = match plugin {
                Some(p) => format!("{p}:{name}"),
                None => name.to_string(),
            };
            let from = match (plugin, e.kind) {
                (Some(p), _) => format!("Plugin: {p}"),
                (None, Kind::Command) => "Command".into(),
                _ => "Skill".into(),
            };
            if !out.iter().any(|c| c.name == name) {
                out.push(Choice { name, description: e.description.clone(), from });
            }
        }
    }
    for (name, description) in builtin {
        if !out.iter().any(|c| c.name == *name) {
            out.push(Choice { name: name.to_string(), description: description.to_string(), from: agent.to_string() });
        }
    }
    out
}

/// What is typed after a leading `/`, while it is still one word (`None` once there is a space
/// or a new line, or without the `/`).
pub fn typed(message: &str) -> Option<&str> {
    let rest = message.strip_prefix('/')?;
    (!rest.contains(char::is_whitespace)).then_some(rest)
}

/// The choices for `query`, best first: names starting with it, then names containing it, then
/// descriptions containing it; at most `max`.
pub fn matching<'a>(choices: &'a [Choice], query: &str, max: usize) -> Vec<&'a Choice> {
    // Nothing typed yet: in their own order (skills, then the agent's commands).
    if query.is_empty() {
        return choices.iter().take(max).collect();
    }
    let q = query.to_lowercase();
    let rank = |c: &Choice| {
        let name = c.name.to_lowercase();
        // A plugin's `plugin:name` also matches by the name after the colon.
        let short = name.rsplit(':').next().unwrap_or(&name).to_string();
        if name.starts_with(&q) || short.starts_with(&q) {
            Some(0)
        } else if name.contains(&q) {
            Some(1)
        } else if c.description.to_lowercase().contains(&q) {
            Some(2)
        } else {
            None
        }
    };
    let mut found: Vec<(u8, &Choice)> = choices.iter().filter_map(|c| rank(c).map(|r| (r, c))).collect();
    found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.len().cmp(&b.1.name.len())).then_with(|| a.1.name.cmp(&b.1.name)));
    found.into_iter().take(max).map(|(_, c)| c).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::{Entry, Group};
    use std::path::PathBuf;

    fn entry(kind: Kind, name: &str, description: &str) -> Entry {
        Entry { kind, name: name.into(), description: description.into(), path: PathBuf::new(), read_only: false }
    }

    fn catalog() -> Catalog {
        Catalog {
            groups: vec![
                Group { key: "project".into(), title: "Project".into(), entries: vec![entry(Kind::Skill, "deploy", "Ship it")], folded: false },
                Group {
                    key: "docs".into(),
                    title: "Docs".into(),
                    entries: vec![entry(Kind::Instructions, "CLAUDE.md", ""), entry(Kind::Command, "review.md", "Review the diff")],
                    folded: false,
                },
                Group { key: "plugin:superpowers".into(), title: "Plugin".into(), entries: vec![entry(Kind::Skill, "brainstorming", "Explore ideas")], folded: true },
            ],
        }
    }

    #[test]
    fn skills_commands_plugins_and_builtins() {
        let all = choices(&catalog(), &[("compact", "Compact the context"), ("deploy", "dup")], "Claude Code");
        let names: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["deploy", "review", "superpowers:brainstorming", "compact"]);
        assert_eq!(all[2].from, "Plugin: superpowers");
        assert_eq!(all[3].from, "Claude Code");
    }

    #[test]
    fn only_a_first_word_after_a_slash_is_looked_up() {
        assert_eq!(typed("/rev"), Some("rev"));
        assert_eq!(typed("/"), Some(""));
        assert_eq!(typed("/review the auth code"), None);
        assert_eq!(typed("look at /etc"), None);
    }

    #[test]
    fn best_matches_first() {
        let all = choices(&catalog(), &[("compact", "Compact the context"), ("context", "Show the context")], "Claude Code");
        let names = |q: &str| matching(&all, q, 8).into_iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        assert_eq!(names("brain"), ["superpowers:brainstorming"]);
        assert_eq!(names("co"), ["compact", "context"]);
        assert_eq!(names("diff"), ["review"]);
        assert_eq!(names(""), ["deploy", "review", "superpowers:brainstorming", "compact", "context"]);
    }
}
