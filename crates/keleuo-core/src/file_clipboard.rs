//! Files on the system clipboard, as the file manager puts them there: Copy in the Files list
//! puts the files themselves (not their names), so Finder, Mail or a chat app pastes the file;
//! Paste there copies in what any app copied. macOS goes through the pasteboard (with
//! JavaScript for Automation, its paths passed as arguments, never in the script), Windows
//! through PowerShell's file drop list, Linux through `wl-copy`/`xclip` as a URI list.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Put `paths` on the clipboard as files.
pub fn copy(paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let words: Vec<String> = paths.iter().map(|p| p.to_string_lossy().to_string()).collect();
    if cfg!(target_os = "macos") {
        let script = "function run(argv) { ObjC.import('AppKit'); var pb = $.NSPasteboard.generalPasteboard; pb.clearContents; \
                      return pb.writeObjects($(argv.map(function (p) { return $.NSURL.fileURLWithPath(p); }))); }";
        let out = Command::new("osascript").args(["-l", "JavaScript", "-e", script]).args(&words).output().map_err(|e| e.to_string())?;
        return if out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true" {
            Ok(())
        } else {
            Err(format!("the clipboard refused it: {}", String::from_utf8_lossy(&out.stderr).trim()))
        };
    }
    if cfg!(windows) {
        let ok = Command::new("powershell").args(["-NoProfile", "-Command", "Set-Clipboard -LiteralPath $args"]).args(&words).status().map_err(|e| e.to_string())?;
        return if ok.success() { Ok(()) } else { Err("PowerShell could not set the clipboard".into()) };
    }
    let list: String = paths.iter().map(|p| format!("{}\r\n", uri(p))).collect();
    for (program, args) in [("wl-copy", vec!["--type", "text/uri-list"]), ("xclip", vec!["-selection", "clipboard", "-t", "text/uri-list", "-i"])] {
        let Ok(mut child) = Command::new(program).args(&args).stdin(Stdio::piped()).spawn() else { continue };
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write as _;
            let _ = stdin.write_all(list.as_bytes());
        }
        return if child.wait().is_ok_and(|s| s.success()) { Ok(()) } else { Err(format!("{program} could not set the clipboard")) };
    }
    Err("Install wl-clipboard or xclip to copy files".into())
}

/// The files on the clipboard (none when it holds something else).
pub fn pasted() -> Vec<PathBuf> {
    let out = if cfg!(target_os = "macos") {
        let script = "ObjC.import('AppKit'); var urls = $.NSPasteboard.generalPasteboard.readObjectsForClassesOptions($([$.NSURL]), $({ NSPasteboardURLReadingFileURLsOnlyKey: true })); \
                      var out = []; for (var i = 0; urls && i < urls.count; i++) out.push(urls.objectAtIndex(i).path.js); out.join('\\n');";
        Command::new("osascript").args(["-l", "JavaScript", "-e", script]).output()
    } else if cfg!(windows) {
        Command::new("powershell").args(["-NoProfile", "-Command", "Get-Clipboard -Format FileDropList | ForEach-Object { $_.FullName }"]).output()
    } else {
        Command::new("wl-paste").args(["--type", "text/uri-list"]).output().or_else(|_| Command::new("xclip").args(["-selection", "clipboard", "-t", "text/uri-list", "-o"]).output())
    };
    let Some(out) = out.ok().filter(|o| o.status.success()) else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| if l.starts_with("file://") { from_uri(l) } else { Some(PathBuf::from(l)) })
        .filter(|p| p.is_absolute() && p.exists())
        .collect()
}

fn uri(p: &Path) -> String {
    let mut out = String::from("file://");
    for b in p.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn from_uri(u: &str) -> Option<PathBuf> {
    let rest = u.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

/// Where a copy of `name` goes in `dir`: its own name when free, else as Finder names copies
/// ("notes copy.txt", "notes copy 2.txt").
pub fn free_name(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (1..)
        .map(|n| dir.join(if n == 1 { format!("{stem} copy{ext}") } else { format!("{stem} copy {n}{ext}") }))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// Copy `from` (a file or a folder, with everything in it) into the folder `dir`; where it went.
pub fn copy_into(from: &Path, dir: &Path) -> Result<PathBuf, String> {
    let name = from.file_name().ok_or("nothing to copy")?.to_string_lossy().to_string();
    if dir.starts_with(from) {
        return Err(format!("{name} can't be copied into itself"));
    }
    let to = free_name(dir, &name);
    copy_all(from, &to).map_err(|e| format!("Could not copy {name}: {e}"))?;
    Ok(to)
}

fn copy_all(from: &Path, to: &Path) -> std::io::Result<()> {
    let kind = std::fs::symlink_metadata(from)?.file_type();
    if kind.is_symlink() {
        let target = std::fs::read_link(from)?;
        #[cfg(unix)]
        return std::os::unix::fs::symlink(target, to);
        #[cfg(not(unix))]
        return std::fs::copy(from, to).map(|_| ());
    }
    if kind.is_dir() {
        std::fs::create_dir(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_all(&entry.path(), &to.join(entry.file_name()))?;
        }
        return Ok(());
    }
    std::fs::copy(from, to).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_are_named_as_finder_names_them_and_folders_come_whole() {
        let dir = std::env::temp_dir().join(format!("keleuo-paste-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/inner")).unwrap();
        std::fs::write(dir.join("notes.txt"), "n").unwrap();
        std::fs::write(dir.join("src/inner/a.rs"), "a").unwrap();
        assert_eq!(copy_into(&dir.join("notes.txt"), &dir).unwrap(), dir.join("notes copy.txt"));
        assert_eq!(copy_into(&dir.join("notes.txt"), &dir).unwrap(), dir.join("notes copy 2.txt"));
        let to = copy_into(&dir.join("src"), &dir).unwrap();
        assert_eq!(std::fs::read_to_string(to.join("inner/a.rs")).unwrap(), "a");
        assert!(copy_into(&dir.join("src"), &dir.join("src/inner")).is_err(), "into itself");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paths_go_to_uris_and_back() {
        let p = Path::new("/tmp/my notes/ä.txt");
        assert_eq!(from_uri(&uri(p)).unwrap(), p);
        assert_eq!(free_name(Path::new("/nonexistent-dir"), ".env"), Path::new("/nonexistent-dir/.env"));
    }
}
