//! Tab names: an automatic name until the user gives one, and the user's names kept on disk.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The automatic name of a terminal tab: the program running in it when that is not the shell,
/// else the folder and its git branch.
pub fn auto_name(
    folder: &Path,
    branch: Option<&str>,
    program: Option<&str>,
    shell: &str,
) -> String {
    if let Some(program) = program.filter(|p| !p.is_empty() && !is_shell(p, shell)) {
        return program.to_string();
    }
    let name = folder
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| folder.to_string_lossy().to_string());
    match branch {
        Some(b) if !b.is_empty() => format!("{name} ({b})"),
        _ => name,
    }
}

fn is_shell(program: &str, shell: &str) -> bool {
    let base = |s: &str| {
        s.rsplit('/')
            .next()
            .unwrap_or(s)
            .trim_start_matches('-')
            .to_string()
    };
    let p = base(program);
    p == base(shell)
        || matches!(
            p.as_str(),
            "zsh" | "bash" | "fish" | "sh" | "login" | "pwsh" | "powershell.exe"
        )
}

/// Names the user gave tabs, by key (`<project path>#<tab id>`), saved as JSON.
#[derive(Debug, Default)]
pub struct NameStore {
    path: Option<PathBuf>,
    names: BTreeMap<String, String>,
}

impl NameStore {
    /// The store in the user's config folder (`.../keleion/tab-names.json`).
    pub fn open_default() -> Self {
        Self::open(crate::settings::config_dir().map(|d| d.join("tab-names.json")))
    }

    /// Load from `path` (missing or unreadable means empty). `None` keeps names in memory only.
    pub fn open(path: Option<PathBuf>) -> Self {
        let names = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Self { path, names }
    }

    pub fn key(project: &Path, tab: &str) -> String {
        format!("{}#{tab}", project.display())
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.names.get(key).map(String::as_str)
    }

    /// Set a manual name; an empty name resets to automatic. Returns the save result.
    pub fn set(&mut self, key: &str, name: &str) -> std::io::Result<()> {
        let name = name.trim();
        if name.is_empty() {
            self.names.remove(key);
        } else {
            self.names.insert(key.to_string(), name.to_string());
        }
        self.save()
    }

    pub fn reset(&mut self, key: &str) -> std::io::Result<()> {
        self.set(key, "")
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(&self.names).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_names() {
        let f = Path::new("/home/u/keleion");
        assert_eq!(
            auto_name(f, Some("main"), None, "/bin/zsh"),
            "keleion (main)"
        );
        assert_eq!(auto_name(f, None, Some("zsh"), "/bin/zsh"), "keleion");
        assert_eq!(
            auto_name(f, Some("main"), Some("-zsh"), "/bin/zsh"),
            "keleion (main)"
        );
        assert_eq!(auto_name(f, Some("main"), Some("less"), "/bin/zsh"), "less");
    }

    #[test]
    fn names_persist_and_reset() {
        let path = std::env::temp_dir().join(format!("wb-names-{}/names.json", std::process::id()));
        let key = NameStore::key(Path::new("/p"), "term-1");
        let mut store = NameStore::open(Some(path.clone()));
        store.set(&key, "  build box  ").unwrap();
        assert_eq!(
            NameStore::open(Some(path.clone())).get(&key),
            Some("build box")
        );
        store.reset(&key).unwrap();
        assert_eq!(NameStore::open(Some(path.clone())).get(&key), None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
