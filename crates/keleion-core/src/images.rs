//! Images the person sends or that appear in a session's history, kept as files in the app's
//! cache (one folder per session), so chats can show them after a restart. Nothing leaves the
//! machine. Files older than 30 days are removed.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long a cached image is kept.
pub const KEEP: Duration = Duration::from_secs(30 * 24 * 3600);

/// The images folder in the cache (`KELEION_CACHE_DIR` when set, for tests).
pub fn root() -> Option<PathBuf> {
    match crate::env_var_os("CACHE_DIR") {
        Some(d) if !d.is_empty() => Some(PathBuf::from(d).join("images")),
        _ => dirs::cache_dir().map(|d| d.join("keleion").join("images")),
    }
}

/// The folder for one session's images (`new` before it has an id).
pub fn session_dir(session: Option<&str>) -> Option<PathBuf> {
    let name: String = session.unwrap_or("new").chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    root().map(|r| r.join(if name.is_empty() { "new".into() } else { name }))
}

/// The file extension for a media type.
pub fn extension(media_type: &str) -> &'static str {
    match media_type {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "png",
    }
}

/// Store `bytes` in `dir`, named by their content (the same image is kept once).
pub fn store(dir: &Path, bytes: &[u8], ext: &str) -> std::io::Result<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    let path = dir.join(format!("{:016x}.{ext}", h.finish()));
    if !path.exists() {
        std::fs::create_dir_all(dir)?;
        std::fs::write(&path, bytes)?;
    }
    Ok(path)
}

/// Decode standard base64 (as in a session file's image blocks).
pub fn base64(text: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let bytes: Vec<u8> = text.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            n |= val(*c)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

/// Remove cached images older than [`KEEP`], and folders left empty. Returns how many files
/// went.
pub fn clean_old(root: &Path, now: SystemTime) -> usize {
    let mut removed = 0;
    let Ok(dirs) = std::fs::read_dir(root) else { return 0 };
    for dir in dirs.flatten().filter(|d| d.path().is_dir()) {
        let Ok(files) = std::fs::read_dir(dir.path()) else { continue };
        for f in files.flatten() {
            let old = f
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .is_some_and(|age| age > KEEP);
            if old && std::fs::remove_file(f.path()).is_ok() {
                removed += 1;
            }
        }
        let _ = std::fs::remove_dir(dir.path());
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64() {
        assert_eq!(base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64("aGVsbG8hIQ==").unwrap(), b"hello!!");
        assert!(base64("a*b").is_none());
    }

    #[test]
    fn stores_once_and_cleans_after_thirty_days() {
        let root = std::env::temp_dir().join(format!("keleion-img-{}", std::process::id()));
        let dir = root.join("s1");
        let a = store(&dir, b"one", "png").unwrap();
        assert_eq!(store(&dir, b"one", "png").unwrap(), a);
        assert_eq!(clean_old(&root, SystemTime::now()), 0);
        assert_eq!(clean_old(&root, SystemTime::now() + KEEP + Duration::from_secs(60)), 1);
        assert!(!dir.exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
