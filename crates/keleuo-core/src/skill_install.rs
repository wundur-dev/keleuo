//! Installing skills from a git repository (see `docs/proposals/skills-installer.md`).
//!
//! A skill is a folder with a `SKILL.md` and whatever files it needs; Claude Code, Cursor and
//! Codex read the same format from different folders. Installing fetches the repository (a
//! shallow clone, with the person's own git credentials), lists the skills in it, and copies
//! each chosen one into the folder each chosen agent reads first ([`Backend::skill_folders`]),
//! skipping an agent that already reads another's copy. A record of what was installed, from
//! where, at which commit and to which folders, lets Keleuo update and remove it later. Nothing
//! is run: installing copies files.
//!
//! [`Backend::skill_folders`]: crate::agent::Backend::skill_folders

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::host::Host;

/// Where a skill is installed: one project, or every project of the person's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Project,
    User,
}

/// Where to fetch skills from: a repository, optionally a branch or tag and a folder in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceSpec {
    pub url: String,
    pub rev: Option<String>,
    pub path: String,
    /// How to show it: `owner/repo` for GitHub, else the URL.
    pub label: String,
}

/// Read what the person typed: a git URL (`https://…`, `git@…`, `ssh://…`, a local folder), a
/// GitHub page of a folder (`https://github.com/o/r/tree/branch/path`), or `owner/repo`
/// (GitHub), each optionally followed by `/path/in/repo` (the short form) and `@rev`.
pub fn parse_source(input: &str) -> Option<SourceSpec> {
    let input = input.trim().trim_end_matches('/');
    if input.is_empty() {
        return None;
    }
    // A GitHub page: https://github.com/o/r[/tree/rev/path]
    if let Some(rest) = input.strip_prefix("https://github.com/").or_else(|| input.strip_prefix("http://github.com/")) {
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() < 2 {
            return None;
        }
        let (owner, repo) = (parts[0], parts[1].trim_end_matches(".git"));
        let (rev, path) = match parts.get(2) {
            Some(&"tree") | Some(&"blob") if parts.len() >= 4 => (Some(parts[3].to_string()), parts[4..].join("/")),
            _ => (None, String::new()),
        };
        return Some(SourceSpec { url: format!("https://github.com/{owner}/{repo}.git"), rev, path, label: format!("{owner}/{repo}") });
    }
    let (main, rev) = match input.rsplit_once('@') {
        // `git@host:…` is a URL, not a rev.
        Some((m, r)) if !m.is_empty() && !r.contains(':') && !r.contains('/') && !input.starts_with("git@") => {
            (m, Some(r.to_string()))
        }
        Some((m, r)) if input.starts_with("git@") && m.contains(':') && !r.contains(':') && m.matches('@').count() == 1 => {
            (m, Some(r.to_string()))
        }
        _ => (input, None),
    };
    let is_url = main.contains("://") || main.starts_with("git@") || main.starts_with('/') || main.starts_with('~');
    if is_url {
        return Some(SourceSpec { url: main.to_string(), rev, path: String::new(), label: main.to_string() });
    }
    // owner/repo[/path…]
    let parts: Vec<&str> = main.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() < 2 || parts[..2].iter().any(|p| p.contains(' ')) {
        return None;
    }
    Some(SourceSpec {
        url: format!("https://github.com/{}/{}.git", parts[0], parts[1].trim_end_matches(".git")),
        rev,
        path: parts[2..].join("/"),
        label: format!("{}/{}", parts[0], parts[1].trim_end_matches(".git")),
    })
}

/// A repository fetched into a temporary folder (removed when this is dropped).
pub struct Fetched {
    pub dir: PathBuf,
    pub commit: String,
    pub spec: SourceSpec,
}

impl Drop for Fetched {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn git(dir: Option<&Path>, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    // Never stop to ask for a password in a window nobody sees.
    cmd.env("GIT_TERMINAL_PROMPT", "0").args(args);
    let out = cmd.output().context("could not run git (is it installed?)")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("{}", err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("git failed").trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Fetch `spec` (blocking: run it off the UI thread): a shallow clone of the one revision, or
/// for a commit id, the repository then that commit.
pub fn fetch(spec: &SourceSpec) -> Result<Fetched> {
    let dir = std::env::temp_dir().join(format!("keleuo-skill-{}-{}", std::process::id(), unique()));
    let dir_s = dir.to_string_lossy().to_string();
    let fetched = Fetched { dir: dir.clone(), commit: String::new(), spec: spec.clone() };
    let is_commit = spec.rev.as_ref().is_some_and(|r| r.len() >= 7 && r.chars().all(|c| c.is_ascii_hexdigit()));
    match &spec.rev {
        Some(rev) if !is_commit => git(None, &["clone", "--depth", "1", "--branch", rev, "--", &spec.url, &dir_s])?,
        Some(rev) => {
            git(None, &["clone", "--filter=blob:none", "--no-checkout", "--", &spec.url, &dir_s])?;
            git(Some(&dir), &["checkout", "--quiet", rev])?
        }
        None => git(None, &["clone", "--depth", "1", "--", &spec.url, &dir_s])?,
    };
    let mut fetched = fetched;
    fetched.commit = git(Some(&dir), &["rev-parse", "HEAD"])?;
    Ok(fetched)
}

fn unique() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

/// A skill found in a fetched repository.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// Its `name:` (what the agents call it), else its folder's name.
    pub name: String,
    pub description: String,
    /// The folder name it is installed under.
    pub folder: String,
    /// Its folder in the fetched copy, and in the repository.
    pub dir: PathBuf,
    pub rel: String,
    /// Its files, relative to its folder.
    pub files: Vec<String>,
    pub skill_md: String,
    /// Files an agent could run (scripts, programs): said before installing.
    pub runnable: Vec<String>,
}

const SKIP: [&str; 3] = [".git", "node_modules", ".github"];

/// Every skill (a folder holding `SKILL.md`) in `fetched`, under its folder `path`.
pub fn find_skills(fetched: &Fetched) -> Vec<Found> {
    let root = fetched.dir.join(&fetched.spec.path);
    let mut out = Vec::new();
    let mut dirs = vec![root.clone()];
    while let Some(dir) = dirs.pop() {
        let skill = dir.join("SKILL.md");
        if skill.is_file() {
            if let Some(f) = found(&fetched.dir, &dir, &fetched.spec) {
                out.push(f);
            }
            // A skill's own folders are its files, not more skills.
            continue;
        }
        let Ok(read) = std::fs::read_dir(&dir) else { continue };
        for e in read.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if e.path().is_dir() && !SKIP.contains(&name.as_str()) {
                dirs.push(e.path());
            }
        }
    }
    out.sort_by_key(|f| f.name.to_lowercase());
    out
}

fn found(repo: &Path, dir: &Path, spec: &SourceSpec) -> Option<Found> {
    let skill_md = std::fs::read_to_string(dir.join("SKILL.md")).ok()?;
    let fm = crate::frontmatter::parse(&skill_md);
    let rel = dir.strip_prefix(repo).ok()?.to_string_lossy().to_string();
    // A skill at the repository's top is named for the repository.
    let folder_name = dir
        .file_name()
        .filter(|_| !rel.is_empty())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| spec.label.rsplit('/').next().unwrap_or("skill").to_string());
    let name = fm.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| folder_name.clone());
    let folder = safe_folder(&name).unwrap_or(folder_name);
    let mut files = Vec::new();
    let mut runnable = Vec::new();
    for f in walk(dir) {
        let r = f.strip_prefix(dir).ok()?.to_string_lossy().to_string();
        if is_runnable(&f) {
            runnable.push(r.clone());
        }
        files.push(r);
    }
    files.sort();
    runnable.sort();
    Some(Found { name, description: fm.description.unwrap_or_default(), folder, dir: dir.to_path_buf(), rel, files, skill_md, runnable })
}

/// A folder name from a skill's name: letters, digits, `-`, `_`, `.`.
fn safe_folder(name: &str) -> Option<String> {
    let s: String = name.trim().chars().map(|c| if c.is_alphanumeric() || "-_.".contains(c) { c } else { '-' }).collect();
    let s = s.trim_matches(|c| c == '-' || c == '.').to_string();
    (!s.is_empty()).then_some(s)
}

/// A skill's files under `dir`: not hidden ones (`.git`, `.github`, a plugin's manifest
/// folders: the repository's, not the skill's, when a skill is a repository's top folder).
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        let Ok(read) = std::fs::read_dir(&d) else { continue };
        for e in read.flatten() {
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

fn is_runnable(path: &Path) -> bool {
    let script = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(e, "sh" | "bash" | "zsh" | "py" | "js" | "mjs" | "ts" | "rb" | "pl" | "ps1" | "bat" | "cmd" | "exe")
    });
    #[cfg(unix)]
    let exec = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    };
    #[cfg(not(unix))]
    let exec = false;
    script || exec
}

/// Copy `skill` into `folder` (an agent's skills folder) as `folder/<skill.folder>`, through
/// `host` (on this machine, keeping files' modes, so a script stays runnable). Returns the
/// copy's folder and the hash of what was written.
pub fn copy_into(host: &dyn Host, skill: &Found, folder: &Path) -> Result<(PathBuf, String)> {
    let target = folder.join(&skill.folder);
    for rel in &skill.files {
        let from = skill.dir.join(rel);
        let to = target.join(rel);
        if host.is_remote() {
            if let Some(parent) = to.parent() {
                host.create_dir_all(parent)?;
            }
            host.write(&to, &std::fs::read(&from)?)?;
        } else {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&from, &to).with_context(|| format!("could not copy {rel}"))?;
        }
    }
    let hash = hash_of(host, &target)?;
    Ok((target, hash))
}

/// A hash of every file under `dir` (names and contents), read through `host`: to tell later
/// whether an installed copy was edited.
pub fn hash_of(host: &dyn Host, dir: &Path) -> Result<String> {
    use md5::{Digest, Md5};
    let mut files = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for e in host.read_dir(&d)? {
            let p = d.join(&e.name);
            if e.is_dir {
                dirs.push(p);
            } else {
                files.push(p);
            }
        }
    }
    files.sort();
    let mut h = Md5::new();
    for f in files {
        h.update(f.strip_prefix(dir).unwrap_or(&f).to_string_lossy().as_bytes());
        h.update(b"\0");
        h.update(host.read(&f)?);
        h.update(b"\0");
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

// ----- the record -----

/// One installed copy: which agent it is for and where it is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstalledCopy {
    pub agent: String,
    pub dir: PathBuf,
    /// The hash of the files as installed (see [`hash_of`]).
    pub hash: String,
}

/// A skill Keleuo installed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Installed {
    pub name: String,
    pub folder: String,
    /// What was typed to fetch it, and the repository, folder and commit it came from.
    pub source: String,
    pub url: String,
    pub path: String,
    pub commit: String,
    pub installed: String,
    pub scope: Scope,
    /// The project, for a project install.
    pub project: Option<PathBuf>,
    /// The machine the copies are on (`local`, or the SSH host).
    pub host: String,
    pub copies: Vec<InstalledCopy>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub version: u32,
    pub skills: Vec<Installed>,
}

fn record_file() -> Option<PathBuf> {
    crate::settings::config_dir().map(|d| d.join("installed-skills.json"))
}

impl Record {
    pub fn load() -> Record {
        record_file()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Record { version: 1, skills: Vec::new() })
    }

    pub fn save(&self) -> Result<()> {
        let path = record_file().ok_or_else(|| anyhow!("no settings folder"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// The installed skill one of whose copies is at `dir` (on `host`).
    pub fn holding(&self, host: &str, dir: &Path) -> Option<&Installed> {
        self.skills.iter().find(|s| s.host == host && s.copies.iter().any(|c| dir.starts_with(&c.dir)))
    }

    /// Add `skill`, replacing an earlier install with a copy in any of the same folders.
    pub fn put(&mut self, skill: Installed) {
        self.skills.retain(|s| !(s.host == skill.host && s.copies.iter().any(|c| skill.copies.iter().any(|n| n.dir == c.dir))));
        self.skills.push(skill);
    }
}

/// Remove an installed skill's copies (only the folders its record lists) and its record.
pub fn remove(host: &dyn Host, skill: &Installed) -> Result<()> {
    for c in &skill.copies {
        if host.exists(&c.dir) {
            host.remove_dir_all(&c.dir).with_context(|| format!("could not remove {}", c.dir.display()))?;
        }
    }
    let mut record = Record::load();
    record.skills.retain(|s| !(s.host == skill.host && s.copies == skill.copies));
    record.save()
}

/// Rename the skill whose file is `skill_md` (`<folder>/SKILL.md`): its folder, and the
/// `name:` in its header (what agents call it). The install record follows. Returns the new
/// SKILL.md.
pub fn rename_skill(host: &dyn Host, skill_md: &Path, new_name: &str) -> Result<PathBuf> {
    let name = new_name.trim();
    let folder = safe_folder(name).ok_or_else(|| anyhow!("give it a name"))?;
    let old_dir = skill_md.parent().ok_or_else(|| anyhow!("not a skill's file"))?;
    let new_dir = old_dir.with_file_name(&folder);
    if new_dir != old_dir && host.exists(&new_dir) {
        bail!("there is already a skill folder called {folder}");
    }
    let text = host.read_to_string(skill_md)?;
    host.write(skill_md, with_name(&text, name).as_bytes())?;
    if new_dir != old_dir {
        host.rename(old_dir, &new_dir).with_context(|| format!("could not rename the folder to {folder}"))?;
    }
    let mut record = Record::load();
    let mut changed = false;
    for s in &mut record.skills {
        for c in &mut s.copies {
            if c.dir == old_dir {
                c.dir = new_dir.clone();
                s.name = name.to_string();
                s.folder = folder.clone();
                changed = true;
            }
        }
    }
    if changed {
        record.save()?;
    }
    Ok(new_dir.join("SKILL.md"))
}

/// `text` (a SKILL.md) with its header's `name:` set to `name` (a header made when it has
/// none).
fn with_name(text: &str, name: &str) -> String {
    let line = format!("name: {name}");
    let Some(rest) = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) else {
        return format!("---\n{line}\n---\n{text}");
    };
    let Some(end) = rest.find("\n---") else { return format!("---\n{line}\n---\n{text}") };
    let (header, after) = rest.split_at(end);
    let mut found = false;
    let mut lines: Vec<String> = header
        .lines()
        .map(|l| {
            if !found && l.trim_start().starts_with("name:") {
                found = true;
                line.clone()
            } else {
                l.to_string()
            }
        })
        .collect();
    if !found {
        lines.insert(0, line);
    }
    format!("---\n{}{after}", lines.join("\n"))
}

/// Take the skill whose file is `skill_md` away: its folder goes in the Trash on this machine
/// (true), or is deleted over SSH, where there is none (false). The install record follows.
pub fn discard_skill(host: &dyn Host, skill_md: &Path) -> Result<bool> {
    let dir = skill_md.parent().ok_or_else(|| anyhow!("not a skill's file"))?;
    let trashed = if host.is_remote() {
        host.remove_dir_all(dir)?;
        false
    } else {
        trash::delete(dir).map_err(|e| anyhow!("could not move it to the Trash: {e}"))?;
        true
    };
    let mut record = Record::load();
    let before = record.skills.len();
    for s in &mut record.skills {
        s.copies.retain(|c| c.dir != dir);
    }
    record.skills.retain(|s| !s.copies.is_empty());
    if record.skills.len() != before {
        record.save()?;
    }
    Ok(trashed)
}

/// The copies of `skill` that were edited since they were installed.
pub fn edited_copies(host: &dyn Host, skill: &Installed) -> Vec<PathBuf> {
    skill
        .copies
        .iter()
        .filter(|c| host.exists(&c.dir) && hash_of(host, &c.dir).is_ok_and(|h| h != c.hash))
        .map(|c| c.dir.clone())
        .collect()
}

/// Today, as `YYYY-MM-DD` (UTC).
pub fn today() -> String {
    let days = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() / 86_400).unwrap_or(0) as i64;
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::LocalHost;

    #[test]
    fn a_skills_name_is_set_in_its_header() {
        assert_eq!(with_name("---\nname: old\ndescription: d\n---\nbody", "new one"), "---\nname: new one\ndescription: d\n---\nbody");
        assert_eq!(with_name("---\ndescription: d\n---\nbody", "x"), "---\nname: x\ndescription: d\n---\nbody");
        assert_eq!(with_name("just text", "x"), "---\nname: x\n---\njust text");
    }

    #[test]
    fn a_skill_is_renamed_folder_and_name() {
        let root = std::env::temp_dir().join(format!("keleuo-skill-rename-{}", unique()));
        let dir = root.join("skills/new-skill");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "---\nname: new-skill\ndescription: d\n---\nbody").unwrap();
        let made = rename_skill(&LocalHost, &dir.join("SKILL.md"), "Tidy prose").unwrap();
        assert_eq!(made, root.join("skills/Tidy-prose/SKILL.md"));
        assert!(!dir.exists());
        assert!(std::fs::read_to_string(&made).unwrap().starts_with("---\nname: Tidy prose\n"));
        // A name already taken by another folder is refused.
        std::fs::create_dir_all(root.join("skills/other")).unwrap();
        assert!(rename_skill(&LocalHost, &made, "other").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sources_are_read_in_their_usual_forms() {
        let s = parse_source("blader/humanizer").unwrap();
        assert_eq!((s.url.as_str(), s.label.as_str(), s.path.as_str(), s.rev), ("https://github.com/blader/humanizer.git", "blader/humanizer", "", None));
        let s = parse_source("anthropics/skills/skills/pdf@main").unwrap();
        assert_eq!((s.path.as_str(), s.rev.as_deref()), ("skills/pdf", Some("main")));
        let s = parse_source("https://github.com/anthropics/skills/tree/main/skills/pdf").unwrap();
        assert_eq!((s.url.as_str(), s.rev.as_deref(), s.path.as_str()), ("https://github.com/anthropics/skills.git", Some("main"), "skills/pdf"));
        let s = parse_source("git@gitlab.com:wundur-ai/skills.git").unwrap();
        assert_eq!((s.url.as_str(), s.rev), ("git@gitlab.com:wundur-ai/skills.git", None));
        let s = parse_source("https://gitlab.com/a/b.git@v2").unwrap();
        assert_eq!((s.url.as_str(), s.rev.as_deref()), ("https://gitlab.com/a/b.git", Some("v2")));
        assert_eq!(parse_source("nope"), None);
        assert_eq!(parse_source("  "), None);
    }

    /// A local repository with two skills, fetched, found, copied, hashed and removed.
    #[test]
    fn a_repository_is_fetched_its_skills_found_and_copied() {
        let root = std::env::temp_dir().join(format!("keleuo-skill-install-test-{}", unique()));
        let repo = root.join("repo");
        let skill = |dir: &str, md: &str| {
            std::fs::create_dir_all(repo.join(dir)).unwrap();
            std::fs::write(repo.join(dir).join("SKILL.md"), md).unwrap();
        };
        skill("skills/tidy", "---\nname: tidy\ndescription: Tidies text\n---\nDo it.");
        std::fs::create_dir_all(repo.join("skills/tidy/.github")).unwrap();
        std::fs::write(repo.join("skills/tidy/.github/ci.yml"), "x").unwrap();
        skill("skills/run-it", "---\nname: run it\ndescription: Runs\n---\nRun scripts/go.sh");
        std::fs::create_dir_all(repo.join("skills/run-it/scripts")).unwrap();
        std::fs::write(repo.join("skills/run-it/scripts/go.sh"), "echo hi").unwrap();
        for args in [&["init", "-q"][..], &["add", "."], &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init"]] {
            git(Some(&repo), args).unwrap();
        }
        let spec = parse_source(&repo.to_string_lossy()).unwrap();
        let fetched = fetch(&spec).unwrap();
        assert_eq!(fetched.commit.len(), 40);
        let found = find_skills(&fetched);
        let names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["run it", "tidy"]);
        assert_eq!(found[0].folder, "run-it");
        assert_eq!(found[0].runnable, ["scripts/go.sh"]);
        assert_eq!(found[1].description, "Tidies text");
        assert_eq!(found[1].files, ["SKILL.md"], "hidden files are not the skill's");

        let agent_folder = root.join("home/.claude/skills");
        let (dir, hash) = copy_into(&LocalHost, &found[0], &agent_folder).unwrap();
        assert!(dir.join("scripts/go.sh").is_file() && dir.join("SKILL.md").is_file());
        assert_eq!(hash_of(&LocalHost, &dir).unwrap(), hash);
        let installed = Installed {
            name: found[0].name.clone(),
            folder: found[0].folder.clone(),
            source: spec.label.clone(),
            url: spec.url.clone(),
            path: found[0].rel.clone(),
            commit: fetched.commit.clone(),
            installed: today(),
            scope: Scope::User,
            project: None,
            host: "local".into(),
            copies: vec![InstalledCopy { agent: "claude-code".into(), dir: dir.clone(), hash }],
        };
        assert!(edited_copies(&LocalHost, &installed).is_empty());
        std::fs::write(dir.join("SKILL.md"), "changed").unwrap();
        assert_eq!(edited_copies(&LocalHost, &installed), [dir.clone()]);
        let fetched_dir = fetched.dir.clone();
        drop(fetched);
        assert!(!fetched_dir.exists(), "the fetched copy goes when done with");
        let _ = std::fs::remove_dir_all(&root);
    }
}
