//! Keeping an agent's writes inside its session's folder. The agent reads anywhere; it writes
//! only to the folders a [`WritePolicy`] allows: the session's folder, its repository's shared
//! git folder, temporary folders and tool caches, and the agent's own state. Everything it
//! starts (a shell, git, a build) inherits the same limit, so a command run in the wrong
//! session cannot change another project.
//!
//! [`Sandbox`] is the interface; each system has its own way:
//!
//! - macOS: the system sandbox (`sandbox-exec` with a generated profile: everything allowed,
//!   file writes denied, then allowed under the policy's folders).
//! - Linux: bubblewrap (`bwrap`): the whole system mounted read-only, the policy's folders
//!   mounted writable over it. Unavailable when `bwrap` is not installed.
//! - Windows: not supported yet ([`Unsupported`]); the agent runs unconfined.
//!
//! Agents on another machine (over SSH) run unconfined too: the limit would have to be set up
//! there.

use std::path::{Path, PathBuf};

use crate::host::CommandSpec;

/// Where an agent may write. Everything else it may read but not change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WritePolicy {
    /// Folders written anywhere inside.
    pub folders: Vec<PathBuf>,
    /// Files written by path prefix: the file and its siblings sharing the prefix (an agent
    /// writing `~/.claude.json` through `~/.claude.json.tmp.123` and renaming it).
    pub file_prefixes: Vec<PathBuf>,
}

/// Whether this system can confine an agent, and if not, why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Support {
    Available,
    Unavailable(String),
}

/// A way to run a command with its writes limited to a [`WritePolicy`].
pub trait Sandbox: Send + Sync {
    /// What it is called, for Settings ("macOS sandbox").
    fn name(&self) -> &'static str;
    fn support(&self) -> Support;
    /// `command`, run inside the sandbox. Called only when [`Sandbox::support`] is available.
    fn wrap(&self, command: CommandSpec, policy: &WritePolicy) -> CommandSpec;
}

/// This system's sandbox.
pub fn system() -> &'static dyn Sandbox {
    #[cfg(target_os = "macos")]
    {
        &Seatbelt
    }
    #[cfg(target_os = "linux")]
    {
        &Bubblewrap
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        &Unsupported
    }
}

/// `command` confined by `policy`, when there is one and this system can; else as it is.
pub fn apply(command: CommandSpec, policy: Option<&WritePolicy>) -> CommandSpec {
    match policy {
        Some(policy) if system().support() == Support::Available => system().wrap(command, policy),
        _ => command,
    }
}

/// The usual policy for an agent working in `folder`: the folder, its repository's shared git
/// folder (a worktree's lives in the main checkout), temporary folders, tool caches and `/dev`,
/// plus the agent's own state (`state`: folders, or files by prefix) and `extra` folders the
/// person allowed.
pub fn policy_for(folder: &Path, state: &[PathBuf], extra: &[PathBuf]) -> WritePolicy {
    let mut folders = vec![folder.to_path_buf()];
    if let Some(git) = git_common_dir(folder) {
        folders.push(git);
    }
    folders.push(std::env::temp_dir());
    folders.extend(["/tmp", "/private/tmp", "/var/folders", "/private/var/folders", "/dev"].map(PathBuf::from));
    if let Some(home) = dirs::home_dir() {
        // Where builds and package managers keep what they fetch and make.
        for cache in [
            ".cache", "Library/Caches", ".cargo", ".rustup", ".npm", ".yarn", ".pnpm-store", ".bun", ".gradle", ".m2", "go",
            ".local/share/pnpm", ".local/state",
        ] {
            folders.push(home.join(cache));
        }
        // The login keychain: a program saving a password (an agent signing in) writes its
        // file itself, and reports the refusal as "keychain locked".
        if cfg!(target_os = "macos") {
            folders.push(home.join("Library/Keychains"));
        }
    }
    folders.extend(extra.iter().cloned());
    let mut file_prefixes = Vec::new();
    for p in state {
        // A folder that exists is a folder; anything else is a file and its siblings.
        if p.is_dir() {
            folders.push(p.clone());
        } else {
            file_prefixes.push(p.clone());
        }
    }
    WritePolicy { folders, file_prefixes }
}

/// The policy for an agent that only reads in `folder` (a reviewer): as [`policy_for`], but
/// the folder and its repository's git folder are not writable.
pub fn read_only_policy(folder: &Path, state: &[PathBuf], extra: &[PathBuf]) -> WritePolicy {
    let mut p = policy_for(folder, state, extra);
    let git = git_common_dir(folder);
    p.folders.retain(|f| !f.starts_with(folder) && Some(f) != git.as_ref());
    p
}

/// A refused write in a command's output: `Some(path)` when the output says where, `Some(None)`
/// when it does not; `None` when nothing was refused. The forms differ by what refused it: zsh
/// (`operation not permitted: /p`), Node (`EPERM: operation not permitted, open '/p'`), and
/// `touch`, `cp` and the like (`/p: Operation not permitted`).
pub fn refused(output: &str) -> Option<Option<PathBuf>> {
    let line = output.lines().find(|l| l.to_lowercase().contains("operation not permitted"))?;
    let lower = line.to_lowercase();
    let at = lower.find("operation not permitted")?;
    let after = &line[at + "operation not permitted".len()..];
    // After it: `: /p` or `, open '/p'`.
    let quoted = after.split('\'').nth(1).filter(|p| p.starts_with('/'));
    let colon = after.strip_prefix(':').map(str::trim).filter(|p| p.starts_with('/'));
    // Before it: `touch: /p: Operation not permitted`.
    let before = line[..at].trim_end().trim_end_matches(':').rsplit(": ").next().map(str::trim).filter(|p| p.starts_with('/'));
    Some(quoted.or(colon).or(before).map(PathBuf::from))
}

/// The repository's shared git folder, when `folder` is in one and it lies outside `folder`.
fn git_common_dir(folder: &Path) -> Option<PathBuf> {
    let out = crate::platform::command("git").arg("-C").arg(folder).args(["rev-parse", "--path-format=absolute", "--git-common-dir"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    (!dir.as_os_str().is_empty() && !dir.starts_with(folder)).then_some(dir)
}

/// A path and, if different, where it really is (`/tmp` is `/private/tmp` on macOS): a
/// sandbox matches the real one.
fn with_real(p: &Path) -> Vec<PathBuf> {
    let mut out = vec![p.to_path_buf()];
    if let Ok(real) = std::fs::canonicalize(p)
        && real != p
    {
        out.push(real);
    }
    out
}

/// macOS: the system sandbox, by `sandbox-exec` and a generated profile.
pub struct Seatbelt;

impl Seatbelt {
    /// The profile: everything allowed, file writes denied, then allowed inside the policy.
    pub fn profile(policy: &WritePolicy) -> String {
        let quote = |p: &Path| format!("\"{}\"", p.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\""));
        let mut allow = Vec::new();
        for f in &policy.folders {
            for p in with_real(f) {
                allow.push(format!("(subpath {})", quote(&p)));
            }
        }
        for f in &policy.file_prefixes {
            // The file (it may not exist yet) and its siblings sharing the prefix.
            let real = f.parent().map(with_real).unwrap_or_default();
            for dir in real {
                let Some(name) = f.file_name() else { continue };
                allow.push(format!("(regex #\"^{}\")", regex_escape(&dir.join(name).to_string_lossy())));
            }
        }
        format!("(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write*\n  {}\n)\n", allow.join("\n  "))
    }
}

/// A path as a literal in a sandbox profile's regex.
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.^$|?*+()[]{}\"".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl Sandbox for Seatbelt {
    fn name(&self) -> &'static str {
        "macOS sandbox"
    }

    fn support(&self) -> Support {
        if !Path::new("/usr/bin/sandbox-exec").exists() {
            Support::Unavailable("sandbox-exec is missing from this Mac.".into())
        } else if already_sandboxed() {
            // A sandbox cannot be applied inside another: Keleuo was started from a confined
            // agent, and what it starts is confined by that already.
            Support::Unavailable("Keleuo itself runs in a sandbox (it was started from a confined agent); agents it starts stay inside that one.".into())
        } else {
            Support::Available
        }
    }

    fn wrap(&self, command: CommandSpec, policy: &WritePolicy) -> CommandSpec {
        let mut args = vec!["-p".to_string(), Self::profile(policy), command.program.to_string_lossy().to_string()];
        args.extend(command.args);
        CommandSpec { program: "/usr/bin/sandbox-exec".into(), args, cwd: command.cwd, env: command.env }
    }
}

/// Whether this process already runs in a macOS sandbox.
#[cfg(target_os = "macos")]
fn already_sandboxed() -> bool {
    unsafe extern "C" {
        // libsandbox (in libSystem): 1 when `pid` runs sandboxed. With no operation it asks
        // only whether there is a sandbox at all.
        fn sandbox_check(pid: i32, operation: *const std::ffi::c_char, kind: i32, ...) -> i32;
    }
    // SAFETY: a query with no operation; the process's own id.
    unsafe { sandbox_check(std::process::id() as i32, std::ptr::null(), 0) == 1 }
}

#[cfg(not(target_os = "macos"))]
fn already_sandboxed() -> bool {
    false
}

/// Linux: bubblewrap, the system read-only and the policy's folders writable.
pub struct Bubblewrap;

impl Bubblewrap {
    /// `bwrap`'s arguments before the command.
    pub fn args(policy: &WritePolicy) -> Vec<String> {
        let mut args: Vec<String> = ["--ro-bind", "/", "/", "--dev-bind", "/dev", "/dev", "--proc", "/proc", "--die-with-parent"]
            .map(String::from)
            .to_vec();
        let mut bind = |p: &Path| {
            // Only what exists can be mounted; a folder made later stays read-only.
            if p.exists() {
                let s = p.to_string_lossy().to_string();
                args.extend(["--bind".to_string(), s.clone(), s]);
            }
        };
        for f in &policy.folders {
            if f != Path::new("/dev") {
                bind(f);
            }
        }
        // A file by prefix: only the file itself (bubblewrap mounts whole paths, so siblings
        // written beside it and renamed over it are not possible).
        for f in &policy.file_prefixes {
            bind(f);
        }
        args.push("--".into());
        args
    }
}

impl Sandbox for Bubblewrap {
    fn name(&self) -> &'static str {
        "bubblewrap"
    }

    fn support(&self) -> Support {
        if crate::host::find_program("bwrap").is_some() {
            Support::Available
        } else {
            Support::Unavailable("Install bubblewrap (bwrap) to keep agents' writes inside their folder.".into())
        }
    }

    fn wrap(&self, command: CommandSpec, policy: &WritePolicy) -> CommandSpec {
        let mut args = Self::args(policy);
        args.push(command.program.to_string_lossy().to_string());
        args.extend(command.args);
        let program = crate::host::find_program("bwrap").unwrap_or_else(|| "bwrap".into());
        CommandSpec { program, args, cwd: command.cwd, env: command.env }
    }
}

/// A system with no sandbox yet (Windows).
pub struct Unsupported;

impl Sandbox for Unsupported {
    fn name(&self) -> &'static str {
        "none"
    }

    fn support(&self) -> Support {
        Support::Unavailable("Not available on this system yet: agents can write outside their folder.".into())
    }

    fn wrap(&self, command: CommandSpec, _: &WritePolicy) -> CommandSpec {
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_writes_are_found_with_their_path_in_each_form() {
        let p = |s: &str| refused(s);
        assert_eq!(p("(eval):1: operation not permitted: /Users/me/x.txt"), Some(Some("/Users/me/x.txt".into())));
        assert_eq!(p("Error: EPERM: operation not permitted, open '/Users/me/site/a.md'"), Some(Some("/Users/me/site/a.md".into())));
        assert_eq!(p("touch: /Users/me/y: Operation not permitted"), Some(Some("/Users/me/y".into())));
        assert_eq!(p("Operation not permitted"), Some(None));
        assert_eq!(p("all good"), None);
    }

    fn policy() -> WritePolicy {
        WritePolicy { folders: vec!["/work/app".into()], file_prefixes: vec!["/home/me/.agent.json".into()] }
    }

    #[test]
    fn the_macos_profile_denies_writes_but_inside_the_policy() {
        let p = Seatbelt::profile(&policy());
        assert!(p.contains("(allow default)"));
        assert!(p.contains("(deny file-write*)"));
        assert!(p.contains("(subpath \"/work/app\")"));
        assert!(p.contains(r#"(regex #"^/home/me/\.agent\.json")"#), "{p}");
    }

    #[test]
    fn a_wrapped_command_keeps_its_folder_and_environment() {
        let cmd = CommandSpec { program: "/bin/agent".into(), args: vec!["-p".into()], cwd: Some("/work/app".into()), env: vec![("A".into(), "1".into())] };
        let wrapped = Seatbelt.wrap(cmd, &policy());
        assert_eq!(wrapped.program, PathBuf::from("/usr/bin/sandbox-exec"));
        assert_eq!(&wrapped.args[2..], ["/bin/agent", "-p"]);
        assert_eq!(wrapped.cwd, Some(PathBuf::from("/work/app")));
        assert_eq!(wrapped.env, vec![("A".to_string(), "1".to_string())]);
    }

    #[test]
    fn bubblewrap_mounts_the_system_read_only_and_existing_policy_folders_writable() {
        let dir = std::env::temp_dir();
        let args = Bubblewrap::args(&WritePolicy { folders: vec![dir.clone(), "/no/such/folder".into()], file_prefixes: vec![] });
        assert_eq!(&args[..3], ["--ro-bind", "/", "/"]);
        let s = dir.to_string_lossy().to_string();
        assert!(args.windows(3).any(|w| w == ["--bind", s.as_str(), s.as_str()]));
        assert!(!args.iter().any(|a| a == "/no/such/folder"));
        assert_eq!(args.last().map(String::as_str), Some("--"));
    }

    /// The real thing: a shell under the macOS sandbox writes in the allowed folder and
    /// not beside it.
    #[cfg(target_os = "macos")]
    #[test]
    #[cfg(target_os = "macos")]
    fn an_agent_may_save_to_the_login_keychain() {
        let home = dirs::home_dir().unwrap();
        let policy = policy_for(Path::new("/tmp/project"), &[], &[]);
        assert!(policy.folders.contains(&home.join("Library/Keychains")), "signing in saves there");
    }

    #[test]
    fn on_macos_writes_outside_the_folder_are_refused() {
        // Run from inside a sandbox (a confined agent's shell), another cannot be applied.
        if already_sandboxed() {
            return;
        }
        let base = std::env::temp_dir().join(format!("keleuo-sandbox-{}", std::process::id()));
        let (inside, outside) = (base.join("inside"), base.join("outside"));
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let policy = WritePolicy { folders: vec![inside.clone()], file_prefixes: vec![] };
        let run = |target: &Path| {
            let cmd = CommandSpec { program: "/bin/sh".into(), args: vec!["-c".into(), format!("echo hi > '{}'", target.join("f").display())], cwd: None, env: vec![] };
            Seatbelt.wrap(cmd, &policy).to_command().status().unwrap().success()
        };
        assert!(run(&inside), "a write inside the folder");
        assert!(!run(&outside), "a write beside it");
        assert!(!outside.join("f").exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
