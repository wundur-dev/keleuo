//! macOS and Linux (see the module above).

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Command;

pub fn command(program: &OsStr) -> Command {
    Command::new(program)
}

/// Through `env -u`, which works the same for a plain process and a pseudo-terminal.
pub fn without_vars(program: PathBuf, args: &[String], unset: &[String]) -> (PathBuf, Vec<String>, Vec<(String, String)>) {
    if unset.is_empty() {
        return (program, args.to_vec(), Vec::new());
    }
    let mut all: Vec<String> = unset.iter().flat_map(|v| ["-u".to_string(), v.clone()]).collect();
    all.push(program.to_string_lossy().to_string());
    all.extend(args.iter().cloned());
    ("/usr/bin/env".into(), all, Vec::new())
}

/// A process group of its own.
pub fn own_group(cmd: &mut Command) -> &mut Command {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0)
}

/// SIGKILL to the whole group (its id is the child's, see [`own_group`]).
pub fn kill_group(child: &mut std::process::Child) {
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: kill(2) with a negative id signals that process group; no memory involved.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    let _ = child.kill();
}

/// A symbolic link.
pub fn link_dir(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}
