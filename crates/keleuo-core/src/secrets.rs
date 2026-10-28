//! Secrets kept in the system's store, not in Keleuo's settings file: the macOS Keychain (by
//! the `security` command), the Secret Service on Linux (by `secret-tool`). Windows is not
//! supported yet. Each secret is named by an account under Keleuo's service.

use std::collections::HashMap;
use std::io;
use std::process::Stdio;
use std::sync::Mutex;

/// The service Keleuo's secrets are kept under.
const SERVICE: &str = "keleuo";

/// Whether this system can keep secrets, and if not, why.
pub fn supported() -> Result<(), String> {
    if cfg!(target_os = "macos") {
        Ok(())
    } else if cfg!(target_os = "linux") {
        crate::host::find_program("secret-tool").map(|_| ()).ok_or_else(|| "Install secret-tool (libsecret) to keep keys.".into())
    } else {
        Err("Keeping keys is not supported on this system yet.".into())
    }
}

/// Which secrets are kept, as last looked up or changed here: asking the store is a process
/// each time (tens of milliseconds), too slow to do while drawing.
static KEPT: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);

fn note(account: &str, kept: bool) {
    if let Ok(mut k) = KEPT.lock() {
        k.get_or_insert_with(HashMap::new).insert(account.to_string(), kept);
    }
}

/// Whether the secret named `account` is kept, as known without asking the store (`None`: not
/// looked up yet; [`kept`] looks).
pub fn known(account: &str) -> Option<bool> {
    KEPT.lock().ok()?.as_ref()?.get(account).copied()
}

/// Whether the secret named `account` is kept: from what is known, else the store (once).
pub fn kept(account: &str) -> bool {
    known(account).unwrap_or_else(|| get(account).is_some())
}

/// The secret named `account`, if there is one.
pub fn get(account: &str) -> Option<String> {
    let value = read(account);
    note(account, value.is_some());
    value
}

fn read(account: &str) -> Option<String> {
    let out = if cfg!(target_os = "macos") {
        crate::platform::command("security").args(["find-generic-password", "-s", SERVICE, "-a", account, "-w"]).stderr(Stdio::null()).output().ok()?
    } else if cfg!(target_os = "linux") {
        crate::platform::command("secret-tool").args(["lookup", "service", SERVICE, "account", account]).stderr(Stdio::null()).output().ok()?
    } else {
        return None;
    };
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string();
    (!value.is_empty()).then_some(value)
}

/// Keep `value` as the secret named `account` (replacing one there).
pub fn set(account: &str, value: &str) -> io::Result<()> {
    supported().map_err(io::Error::other)?;
    let status = if cfg!(target_os = "macos") {
        // -U: update it if it is there. The value goes on the command line, as `security` takes
        // it; it is this user's own process, gone at once.
        crate::platform::command("security")
            .args(["add-generic-password", "-U", "-s", SERVICE, "-a", account, "-w", value])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
    } else {
        // secret-tool reads the value on its standard input.
        let mut child = crate::platform::command("secret-tool")
            .args(["store", "--label", &format!("Keleuo: {account}"), "service", SERVICE, "account", account])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        use std::io::Write;
        child.stdin.take().map(|mut i| i.write_all(value.as_bytes())).transpose()?;
        child.wait()?
    };
    if !status.success() {
        return Err(io::Error::other("the system's key store refused it"));
    }
    note(account, true);
    Ok(())
}

/// Forget the secret named `account`.
pub fn remove(account: &str) {
    note(account, false);
    if cfg!(target_os = "macos") {
        let _ = crate::platform::command("security").args(["delete-generic-password", "-s", SERVICE, "-a", account]).output();
    } else if cfg!(target_os = "linux") {
        let _ = crate::platform::command("secret-tool").args(["clear", "service", SERVICE, "account", account]).output();
    }
}
