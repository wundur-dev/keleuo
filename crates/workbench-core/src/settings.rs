//! What Workbench remembers between runs: recent folders and the layout (column widths, the
//! sidebar folded or not). One small JSON file in the user's config folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Recent folders kept at most.
pub const MAX_RECENT: usize = 12;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub sidebar_width: f32,
    pub claude_width: f32,
    pub sidebar_folded: bool,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sidebar_width: 240.0,
            claude_width: 640.0,
            sidebar_folded: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Most recent first.
    pub recent: Vec<PathBuf>,
    pub layout: Layout,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Settings {
    /// `.../workbench/settings.json` in the user's config folder.
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("workbench/settings.json"))
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
    pub fn add_recent(&mut self, folder: &Path) {
        self.recent.retain(|p| p != folder);
        self.recent.insert(0, folder.to_path_buf());
        self.recent.truncate(MAX_RECENT);
    }

    /// Recent folders that still exist.
    pub fn existing_recent(&self) -> Vec<PathBuf> {
        self.recent.iter().filter(|p| p.is_dir()).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
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
        s.save().unwrap();
        let back = Settings::load(Some(path.clone()));
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
