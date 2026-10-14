//! Known ways a tool call fails for no good reason, spotted in its result, each with fixes
//! native to Claude Code (a hook, a line in CLAUDE.md, the shell's settings) so the agent stops
//! making the mistake, with or without Keleuo. Few and precise: a notice on harmless output
//! would teach people to ignore it. Nothing is written until the person chooses a fix.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::agent::{Issue, IssueFix};

/// zsh refused to run a command: an unquoted `?`, `*` or `[` (a URL's query, say) is a filename
/// pattern to it, and with no file matching it stops (bash would pass the text on).
pub const ZSH_NOMATCH: &str = "zsh-nomatch";

/// Marks what Keleuo wrote, so a fix is not added twice and is easy to find and remove.
const MARK: &str = "keleuo:zsh-nomatch";

const HOOK_FILE: &str = "keleuo-quote-patterns.sh";

/// The hook: refuse a Bash command with an unquoted word holding `?`, `*` or `[` after a URL's
/// scheme, saying why, so the agent quotes it and runs it again at once. It reads the raw
/// JSON (no jq or Python needed): a quoted URL there starts with `\"` or `'`.
const HOOK_SCRIPT: &str = r#"#!/bin/sh
# keleuo:zsh-nomatch. Added by Keleuo: zsh treats an unquoted ?, * or [ as a filename
# pattern and refuses to run the command when nothing matches. This stops such a command
# before it runs and tells the agent to quote it. Delete this file (and its entry in
# settings.json) to remove it.
input=$(cat)
if printf '%s' "$input" | grep -Eq '(^|[ =(])https?://[^ "'"'"'\\]*[][?*]'; then
  echo 'Quote URLs that contain ?, * or [ (for example curl "https://example.com/?q=1"): zsh treats them as filename patterns and will not run the command.' >&2
  exit 2
fi
exit 0
"#;

const CLAUDE_MD_LINE: &str = "- Quote URLs and arguments that contain `?`, `*` or `[` in shell commands: zsh treats them as filename patterns and refuses to run the command when nothing matches.";

const ZSHRC_LINES: &str = "setopt NO_NOMATCH  # keleuo:zsh-nomatch: pass unmatched ?, * and [ through as text, as bash does";

/// The issue in a tool's result, if it is a known one.
pub fn spot(tool: &str, result: &str, project: &Path, home: Option<&Path>) -> Option<Issue> {
    if tool != "Bash" {
        return None;
    }
    let word = result.lines().find_map(|l| {
        let rest = l.split_once(": no matches found: ")?.1;
        let head = l.split(':').next()?;
        (head == "zsh" || head == "(eval)" || head.ends_with("zsh")).then(|| rest.trim().to_string())
    })?;
    Some(Issue {
        kind: ZSH_NOMATCH.into(),
        title: "zsh didn't run this command".into(),
        detail: format!(
            "`{word}` has a `?`, `*` or `[` that zsh treats as a filename pattern. Nothing matched, so the command (or that part of it) never ran, and the agent had to try again."
        ),
        // The hook stops unquoted URLs only (a bare `*.rs` is often meant as a pattern), so it
        // is offered only when a URL was the trouble.
        fixes: fixes(project, home, word.contains("://")),
    })
}

fn fixes(project: &Path, home: Option<&Path>, url: bool) -> Vec<IssueFix> {
    let mut out = vec![
        IssueFix {
            id: "hook-project".into(),
            label: "Add a hook for this project".into(),
            preview: format!(
                "Writes {} and adds it as a PreToolUse hook for Bash in {}. It stops a command with an unquoted URL holding ?, * or [ before it runs, and tells the agent to quote it.",
                project.join(".claude/hooks").join(HOOK_FILE).display(),
                project.join(".claude/settings.json").display()
            ),
        },
        IssueFix {
            id: "claude-md".into(),
            label: "Add a line to this project's CLAUDE.md".into(),
            preview: format!("Adds to {}:\n{CLAUDE_MD_LINE}", project.join("CLAUDE.md").display()),
        },
    ];
    if !url {
        out.remove(0);
    }
    if let Some(home) = home {
        if url {
            out.insert(
                1,
                IssueFix {
                    id: "hook-user".into(),
                    label: "Add the hook for all projects".into(),
                    preview: format!(
                        "The same hook, written to {} and added in {}, for every project.",
                        home.join(".claude/hooks").join(HOOK_FILE).display(),
                        home.join(".claude/settings.json").display()
                    ),
                },
            );
        }
        out.push(IssueFix {
            id: "zshrc".into(),
            label: "Turn off zsh's no-match error".into(),
            preview: format!(
                "Adds to {}:\n{ZSHRC_LINES}\nThis changes your own shell too: an unmatched pattern is passed on as text, as in bash.",
                home.join(".zshrc").display()
            ),
        });
    }
    out
}

/// Apply fix `fix` of issue `kind`; what was done, in a sentence.
pub fn apply(kind: &str, fix: &str, project: &Path, home: Option<&Path>) -> std::io::Result<String> {
    if kind != ZSH_NOMATCH {
        return Err(std::io::Error::other("unknown issue"));
    }
    let home_dir = || home.map(Path::to_path_buf).ok_or_else(|| std::io::Error::other("no home folder"));
    match fix {
        "hook-project" => {
            let script = write_hook(&project.join(".claude/hooks"))?;
            let command = format!("\"$CLAUDE_PROJECT_DIR\"/.claude/hooks/{HOOK_FILE}");
            let added = add_hook(&project.join(".claude/settings.json"), &command)?;
            Ok(done(added, "hook", &script))
        }
        "hook-user" => {
            let home = home_dir()?;
            let script = write_hook(&home.join(".claude/hooks"))?;
            let added = add_hook(&home.join(".claude/settings.json"), &script.display().to_string())?;
            Ok(done(added, "hook", &script))
        }
        "claude-md" => {
            let path = project.join("CLAUDE.md");
            let added = append_once(&path, &format!("<!-- {MARK} -->\n{CLAUDE_MD_LINE}\n"))?;
            Ok(done(added, "line", &path))
        }
        "zshrc" => {
            let path = home_dir()?.join(".zshrc");
            let added = append_once(&path, &format!("{ZSHRC_LINES}\n"))?;
            Ok(done(added, "setting", &path))
        }
        _ => Err(std::io::Error::other("unknown fix")),
    }
}

fn done(added: bool, what: &str, path: &Path) -> String {
    if added {
        format!("Added the {what}: {}.", path.display())
    } else {
        format!("The {what} was there already: {}.", path.display())
    }
}

/// The hook script in `dir`, made runnable.
fn write_hook(dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(HOOK_FILE);
    std::fs::write(&path, HOOK_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(path)
}

/// Add a PreToolUse hook for Bash running `command` to the settings file at `path`, keeping
/// everything else in it. False when one running it is there already.
fn add_hook(path: &Path, command: &str) -> std::io::Result<bool> {
    let mut settings: Value = match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => serde_json::from_str(&text)
            .map_err(|e| std::io::Error::other(format!("{} is not valid JSON ({e}); not changed", path.display())))?,
        _ => json!({}),
    };
    let Some(root) = settings.as_object_mut() else {
        return Err(std::io::Error::other(format!("{} is not a JSON object; not changed", path.display())));
    };
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let list = hooks
        .as_object_mut()
        .ok_or_else(|| std::io::Error::other("\"hooks\" is not an object; not changed"))?
        .entry("PreToolUse")
        .or_insert_with(|| json!([]));
    let groups = list.as_array_mut().ok_or_else(|| std::io::Error::other("\"PreToolUse\" is not a list; not changed"))?;
    let present = groups.iter().any(|g| {
        g.get("hooks").and_then(Value::as_array).is_some_and(|hs| {
            hs.iter().any(|h| h.get("command").and_then(Value::as_str).is_some_and(|c| c.contains(HOOK_FILE)))
        })
    });
    if present {
        return Ok(false);
    }
    groups.push(json!({"matcher": "Bash", "hooks": [{"type": "command", "command": command}]}));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(&settings).map_err(std::io::Error::other)?;
    std::fs::write(path, text + "\n")?;
    Ok(true)
}

/// Add `text` at the end of the file at `path` unless Keleuo's mark is in it. Whether it did.
fn append_once(path: &Path, text: &str) -> std::io::Result<bool> {
    let current = std::fs::read_to_string(path).unwrap_or_default();
    if current.contains(MARK) {
        return Ok(false);
    }
    let sep = if current.is_empty() || current.ends_with('\n') { "" } else { "\n" };
    let gap = if current.is_empty() { "" } else { "\n" };
    std::fs::write(path, format!("{current}{sep}{gap}{text}"))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kl-issue-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn spots_zsh_refusing_a_pattern() {
        let p = Path::new("/w/app");
        let hit = spot("Bash", "out\n(eval):1: no matches found: https://x.dev/?q=1\n", p, Some(Path::new("/h"))).unwrap();
        assert_eq!(hit.kind, ZSH_NOMATCH);
        assert!(hit.detail.contains("https://x.dev/?q=1"));
        assert_eq!(hit.fixes.len(), 4);
        // Not a URL: the hook would not catch it, so it is not offered.
        let glob = spot("Bash", "zsh:1: no matches found: --include=*.rs", p, Some(Path::new("/h"))).unwrap();
        let ids: Vec<&str> = glob.fixes.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["claude-md", "zshrc"]);
        assert!(spot("Bash", "grep: no matches found: in file", p, None).is_none(), "not zsh");
        assert!(spot("Read", "(eval):1: no matches found: x", p, None).is_none(), "not a shell");
        assert!(spot("Bash", "all good", p, None).is_none());
    }

    // The hook is a shell script, for Unix shells.
    #[cfg(unix)]
    #[test]
    fn the_hook_stops_unquoted_urls_only() {
        let d = dir("hook");
        let script = write_hook(&d).unwrap();
        let run = |cmd: &str| {
            let input = json!({"tool_name": "Bash", "tool_input": {"command": cmd}}).to_string();
            let mut child = std::process::Command::new("/bin/sh")
                .arg(&script)
                .stdin(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            use std::io::Write;
            child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
            child.wait_with_output().unwrap().status.code()
        };
        assert_eq!(run("curl -s https://api.github.com/search?q=cmux"), Some(2));
        assert_eq!(run("curl -s \"https://api.github.com/search?q=cmux\""), Some(0));
        assert_eq!(run("curl -s 'https://api.github.com/search?q=cmux'"), Some(0));
        assert_eq!(run("curl -s https://example.com/page"), Some(0));
        assert_eq!(run("ls *.rs"), Some(0), "plain globs are left to the shell");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn fixes_merge_into_settings_and_are_not_added_twice() {
        let project = dir("project");
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        std::fs::write(project.join(".claude/settings.json"), r#"{"permissions":{"allow":["Bash(ls:*)"]}}"#).unwrap();
        let first = apply(ZSH_NOMATCH, "hook-project", &project, None).unwrap();
        assert!(first.starts_with("Added"), "{first}");
        let again = apply(ZSH_NOMATCH, "hook-project", &project, None).unwrap();
        assert!(again.contains("there already"), "{again}");
        let s: Value = serde_json::from_str(&std::fs::read_to_string(project.join(".claude/settings.json")).unwrap()).unwrap();
        assert_eq!(s["permissions"]["allow"][0], "Bash(ls:*)", "the rest is kept");
        assert_eq!(s["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
        assert!(project.join(".claude/hooks").join(HOOK_FILE).exists());
        std::fs::write(project.join("CLAUDE.md"), "# App").unwrap();
        apply(ZSH_NOMATCH, "claude-md", &project, None).unwrap();
        apply(ZSH_NOMATCH, "claude-md", &project, None).unwrap();
        let md = std::fs::read_to_string(project.join("CLAUDE.md")).unwrap();
        assert_eq!(md.matches(MARK).count(), 1);
        assert!(md.starts_with("# App\n\n<!--"), "{md}");
        // Broken JSON is left alone.
        std::fs::write(project.join(".claude/settings.json"), "{ not json").unwrap();
        assert!(apply(ZSH_NOMATCH, "hook-project", &project, None).is_err());
        let _ = std::fs::remove_dir_all(project);
    }
}
