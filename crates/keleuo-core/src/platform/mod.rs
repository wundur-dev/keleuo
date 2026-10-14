//! What differs between operating systems, in one place: the rest of Keleuo asks for what it
//! wants here and never checks which system it runs on. Each system's way is in its own file
//! with the same functions (macOS and Linux share the Unix one).

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Command;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as os;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as os;

/// A program to run behind the window: it shows nothing of its own (on Windows a console
/// program started by a windowed one would otherwise get a console window, and so would every
/// program it starts in turn).
pub fn command(program: impl AsRef<OsStr>) -> Command {
    os::command(program.as_ref())
}

/// How to start `program` with `args` without the inherited variables `unset`: the program,
/// arguments and extra variables to start instead.
pub fn without_vars(program: PathBuf, args: &[String], unset: &[String]) -> (PathBuf, Vec<String>, Vec<(String, String)>) {
    os::without_vars(program, args, unset)
}

/// Start `cmd`'s program as the head of a group of its own, so [`kill_group`] reaches the
/// programs it starts too.
pub fn own_group(cmd: &mut Command) -> &mut Command {
    os::own_group(cmd)
}

/// End `child` and every program it started.
pub fn kill_group(child: &mut std::process::Child) {
    os::kill_group(child)
}
