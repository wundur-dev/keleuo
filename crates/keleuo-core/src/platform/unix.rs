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
