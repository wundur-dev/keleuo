//! Windows (see the module above).

use std::ffi::OsStr;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

/// A console of its own that is never shown, which the programs it starts share.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn command(program: &OsStr) -> Command {
    let mut cmd = Command::new(program);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// There is no `env`: the variables are set empty instead, which the agents (JavaScript, where
/// an empty variable reads as unset) take as absent. A pseudo-terminal cannot remove one either.
pub fn without_vars(program: PathBuf, args: &[String], unset: &[String]) -> (PathBuf, Vec<String>, Vec<(String, String)>) {
    (program, args.to_vec(), unset.iter().map(|v| (v.clone(), String::new())).collect())
}

/// Nothing to set up: [`kill_group`] ends the tree by process id.
pub fn own_group(cmd: &mut Command) -> &mut Command {
    cmd
}

/// `taskkill /T` ends the process and the ones it started.
pub fn kill_group(child: &mut std::process::Child) {
    let _ = command(OsStr::new("taskkill")).args(["/T", "/F", "/PID", &child.id().to_string()]).output();
    let _ = child.kill();
}
