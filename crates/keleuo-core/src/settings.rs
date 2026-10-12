//! What Keleuo remembers between runs: recent folders and the layout (column widths, the
//! sidebar folded or not). One small JSON file in the user's config folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Recent folders kept at most.
pub const MAX_RECENT: usize = 12;

/// Where Keleuo keeps its files: `KELEUO_CONFIG_DIR` when set (tests), else
/// `keleuo` in the user's config folder. The folder of an earlier name of the app
/// (`keleion`, before it `workbench`) is moved there the first time; if it cannot be moved,
/// it is used where it is.
pub fn config_dir() -> Option<PathBuf> {
    static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| match crate::env_var_os("CONFIG_DIR") {
        Some(d) if !d.is_empty() => Some(PathBuf::from(d)),
        _ => dirs::config_dir().map(|d| {
            let earlier = migrate(&d.join("workbench"), &d.join("keleion"));
            migrate(&earlier, &d.join("keleuo"))
        }),
    })
    .clone()
}

/// `new`, after moving `old` there when only `old` exists; `old` when that move fails.
pub fn migrate(old: &Path, new: &Path) -> PathBuf {
    if !new.exists() && old.is_dir() && std::fs::rename(old, new).is_err() {
        return old.to_path_buf();
    }
    new.to_path_buf()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub sidebar_width: f32,
    pub claude_width: f32,
    /// The tool panels (files, skills, the agent's subagents) are hidden: the default, so
    /// the conversation has the room; Cmd+B shows them.
    pub sidebar_folded: bool,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sidebar_width: 240.0,
            claude_width: 640.0,
            sidebar_folded: true,
        }
    }
}

/// A session open in the window, opened again on the next start (closing it removes it;
/// quitting does not).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedSession {
    /// The session's place in the list (its tab name is kept by it).
    pub number: usize,
    pub folder: PathBuf,
    /// The agent (backend id).
    pub backend: String,
    /// Its conversation, once it has one.
    pub id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Most recent first.
    pub recent: Vec<PathBuf>,
    pub layout: Layout,
    /// How new sessions show: `"chat"` (the default) or `"terminal"` (the agent's own
    /// interface, for backends that have one).
    pub default_view: String,
    /// The view each session was last shown in, by session id.
    pub session_views: std::collections::BTreeMap<String, String>,
    /// The session list folded to a rail.
    pub sessions_folded: bool,
    /// The sessions open when the window last changed, in list order.
    pub open_sessions: Vec<SavedSession>,
    /// Which of them was shown (its number).
    pub shown_session: Option<usize>,
    /// When to show what a session costs: `"auto"` (the default: only when the agent is
    /// billed per use, else how full its context is), `"always"` or `"never"`.
    pub cost_display: String,
    /// Light or dark: `"system"` (the default: follow macOS), `"light"` or `"dark"`.
    pub appearance: String,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Settings {
    /// `settings.json` in [`config_dir`].
    pub fn default_path() -> Option<PathBuf> {
        config_dir().map(|d| d.join("settings.json"))
    }

    /// Load from `path`; missing or unreadable gives the defaults. `None` keeps it in memory.
    pub fn load(path: Option<PathBuf>) -> Self {
        let mut s: Settings = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        s.path = path;
        s
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(
            path,
            serde_json::to_string_pretty(self).map_err(std::io::Error::other)?,
        )
    }

    /// Put `folder` first in the recent list.
    /// A folder was renamed: recent folders at or under it follow.
    pub fn folder_renamed(&mut self, old: &Path, new: &Path) {
        for p in &mut self.recent {
            if let Ok(rest) = p.strip_prefix(old) {
                *p = if rest.as_os_str().is_empty() { new.to_path_buf() } else { new.join(rest) };
            }
        }
        let mut seen = std::collections::HashSet::new();
        self.recent.retain(|p| seen.insert(p.clone()));
    }

    pub fn add_recent(&mut self, folder: &Path) {
        self.recent.retain(|p| p != folder);
        self.recent.insert(0, folder.to_path_buf());
        self.recent.truncate(MAX_RECENT);
    }

    /// The view a session opens in: its remembered one, else the default.
    pub fn view_for(&self, session: Option<&str>) -> &str {
        session
            .and_then(|id| self.session_views.get(id))
            .map(String::as_str)
            .unwrap_or(if self.default_view.is_empty() { "chat" } else { &self.default_view })
    }

    /// Remember a session's view (the list is kept to the last few hundred sessions).
    pub fn set_view(&mut self, session: &str, view: &str) {
        self.session_views.insert(session.to_string(), view.to_string());
        while self.session_views.len() > 500 {
            let first = self.session_views.keys().next().cloned().unwrap_or_default();
            self.session_views.remove(&first);
        }
    }

    /// Recent folders that still exist.
    pub fn existing_recent(&self) -> Vec<PathBuf> {
        self.recent.iter().filter(|p| p.is_dir()).cloned().collect()
    }
}

/// A folder as a file name: every character that is not a letter or digit becomes `-`.
pub fn encode_folder(folder: &std::path::Path) -> String {
    folder
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

#[cfg(test)]
mod tests {

    #[test]
    fn recent_folders_follow_a_rename() {
        let mut s = Settings::default();
        for p in ["/w/other", "/w/keleion/sub", "/w/keleion-site", "/w/keleion"] {
            s.add_recent(Path::new(p));
        }
        s.folder_renamed(Path::new("/w/keleion"), Path::new("/w/keleuo"));
        let got: Vec<_> = s.recent.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(got, ["/w/keleuo", "/w/keleion-site", "/w/keleuo/sub", "/w/other"]);
    }

    #[test]
    fn the_old_folder_moves_to_the_new_name() {
        let base = std::env::temp_dir().join(format!("keleuo-migrate-{}", std::process::id()));
        let (old, new) = (base.join("keleion"), base.join("keleuo"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("settings.json"), "{}").unwrap();
        assert_eq!(super::migrate(&old, &new), new);
        assert!(new.join("settings.json").exists() && !old.exists());
        // Once moved, nothing more happens.
        assert_eq!(super::migrate(&old, &new), new);
        let _ = std::fs::remove_dir_all(base);
    }
    use super::*;

    #[test]
    fn recent_and_layout_round_trip() {
        let dir = std::env::temp_dir().join(format!("wb-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        let mut s = Settings::load(Some(path.clone()));
        assert_eq!(s.layout, Layout::default());
        for i in 0..15 {
            s.add_recent(Path::new(&format!("/p/{i}")));
        }
        s.add_recent(Path::new("/p/3"));
        s.layout.claude_width = 700.0;
        s.layout.sidebar_folded = true;
        s.open_sessions = vec![
            SavedSession { number: 2, folder: "/p/a".into(), backend: "claude".into(), id: Some("abc".into()) },
            SavedSession { number: 5, folder: "/p/b".into(), backend: "claude".into(), id: None },
        ];
        s.shown_session = Some(5);
        s.save().unwrap();
        let back = Settings::load(Some(path.clone()));
        assert_eq!(back.open_sessions, s.open_sessions);
        assert_eq!(back.shown_session, Some(5));
        assert_eq!(back.recent.len(), MAX_RECENT);
        assert_eq!(back.recent[0], PathBuf::from("/p/3"));
        assert_eq!(back.recent[1], PathBuf::from("/p/14"));
        assert_eq!(back.recent.iter().filter(|p| p.ends_with("3")).count(), 1);
        assert_eq!(back.layout.claude_width, 700.0);
        assert!(back.layout.sidebar_folded);
        assert!(back.existing_recent().is_empty());
        // Unknown or missing fields fall back to defaults.
        std::fs::write(&path, r#"{"layout":{"claude_width":500}}"#).unwrap();
        let partial = Settings::load(Some(path));
        assert_eq!(partial.layout.claude_width, 500.0);
        assert_eq!(partial.layout.sidebar_width, 240.0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
