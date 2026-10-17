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
    /// The height of the chat's pane of latest messages (shown while scrolled up).
    pub latest_height: f32,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sidebar_width: 240.0,
            claude_width: 640.0,
            sidebar_folded: true,
            latest_height: 220.0,
        }
    }
}

/// When to tell the person through the Dock (a bounce, a count on its icon) that a session
/// needs them. Both on by default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notify {
    /// It waits for a permission answer, or asks something in its terminal.
    pub waiting: bool,
    /// Its turn finished: it waits for the next message.
    pub finished: bool,
}

impl Default for Notify {
    fn default() -> Self {
        Self { waiting: true, finished: true }
    }
}

/// Behaviour that can be turned off in Settings. Everything here is on by default, which is
/// why it is apart from [`Settings`] (whose missing fields read as off).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Open again the sessions that were open when Keleuo quit.
    pub reopen_sessions: bool,
    /// Light Markdown highlighting in the message box.
    pub markdown_marks: bool,
    /// Misspelled words in the message box are underlined as they are typed.
    pub spell_check: bool,
    /// A long paste goes in the message box as a pill, its text kept aside.
    pub paste_pills: bool,
    /// A turn that changed files ends with a card listing them.
    pub turn_card: bool,
    /// Files changed by commands (not the edit tools) are found with git.
    pub command_changes: bool,
    /// A terminal shows what it showed before Keleuo last quit.
    pub keep_terminal_history: bool,
    /// The terminals' font size, in points.
    pub terminal_font_size: f32,
    /// The agents' own interfaces' font size (their Terminal view), in points: larger than
    /// the shells', being where the person reads and types.
    pub agent_font_size: f32,
    /// Experimental: an agent with its own terminal interface runs it for the session's whole
    /// life, Chat drawing from its session file, so switching views is instant. Off: Chat runs
    /// the agent headless and Terminal its interface, one at a time, switching between turns.
    pub hybrid_views: bool,
    /// Scrolled up in a chat, the latest messages stay in view in a pane under it.
    pub latest_pane: bool,
    /// Agents write only inside their session's folder (and their own state, caches and
    /// temporary folders); they read anywhere. See [`crate::sandbox`].
    pub sandbox_writes: bool,
}

/// The terminals' font size when none is set, and the range allowed.
pub const TERMINAL_FONT_SIZE: f32 = 11.0;
/// The agents' interfaces' font size when none is set.
pub const AGENT_FONT_SIZE: f32 = 12.5;
pub const TERMINAL_FONT_SIZES: std::ops::RangeInclusive<f32> = 9.0..=24.0;

impl Default for Prefs {
    fn default() -> Self {
        Self {
            reopen_sessions: true,
            markdown_marks: true,
            spell_check: true,
            paste_pills: true,
            turn_card: true,
            command_changes: true,
            keep_terminal_history: true,
            terminal_font_size: TERMINAL_FONT_SIZE,
            agent_font_size: AGENT_FONT_SIZE,
            hybrid_views: false,
            latest_pane: true,
            sandbox_writes: true,
        }
    }
}

impl Prefs {
    /// The terminal font size, kept in range (a hand-edited file may hold anything).
    /// The agents' interfaces' font size, kept in range.
    pub fn agent_font_size(&self) -> f32 {
        if self.agent_font_size.is_finite() && self.agent_font_size > 0. {
            self.agent_font_size.clamp(*TERMINAL_FONT_SIZES.start(), *TERMINAL_FONT_SIZES.end())
        } else {
            AGENT_FONT_SIZE
        }
    }

    pub fn terminal_font_size(&self) -> f32 {
        if self.terminal_font_size.is_finite() {
            self.terminal_font_size.clamp(*TERMINAL_FONT_SIZES.start(), *TERMINAL_FONT_SIZES.end())
        } else {
            TERMINAL_FONT_SIZE
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
    /// Its agent may write anywhere (else only in its folder; see [`crate::sandbox`]).
    pub writes_anywhere: bool,
    /// Folders outside its own that its agent may write in too.
    pub writable: Vec<PathBuf>,
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
    /// The permission mode new sessions start in (the agent's own mode name: `"default"` asks
    /// before acting, `"auto"`, `"acceptEdits"`, `"plan"`); empty is `"auto"`.
    pub default_permission_mode: String,
    /// The permission mode each session was last set to, by session id.
    pub session_modes: std::collections::BTreeMap<String, String>,
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
    /// The app's font (panels, inputs, menus): a family Keleuo carries, or empty for the
    /// system's.
    pub ui_font: String,
    /// The chat's font (its messages and message box): as `ui_font`; empty: the app's.
    pub chat_font: String,
    /// The terminals' font, and code's in the chat: a monospace Keleuo carries, or empty for
    /// the system's.
    pub terminal_font: String,
    /// How tabs are drawn: `"block"` (the default), `"sheet"` or `"underline"`.
    pub tab_style: String,
    /// Model names typed in beyond a backend's own list, most recent first.
    pub recent_models: Vec<String>,
    /// The message box keeps its proportional font even with a code block in it (by default
    /// it turns monospace while a message holds a ``` fence).
    pub always_proportional: bool,
    /// When to tell the person a session needs them.
    pub notify: Notify,
    /// What can be turned off in Settings.
    pub prefs: Prefs,
    /// Conversations that were working when Keleuo quit, to be told to carry on when it starts
    /// again (the person asked for it on quitting).
    pub continue_on_start: Vec<String>,
    /// Turn summaries by a language model server (off until set up).
    pub summaries: crate::summary::Config,
    /// Tool servers (MCP) every agent gets ([`crate::mcp`]); and each folder's own.
    pub mcp_servers: Vec<crate::mcp::Server>,
    pub folder_mcp: std::collections::BTreeMap<PathBuf, crate::mcp::FolderServers>,
    /// Tools made from command-line programs ([`crate::cli_tools`]); and each folder's own.
    pub cli_tools: Vec<crate::cli_tools::Tool>,
    pub folder_tools: std::collections::BTreeMap<PathBuf, crate::cli_tools::FolderTools>,
    /// Settings this build does not know (written by a newer one), kept as they are so
    /// saving does not drop them.
    #[serde(flatten)]
    pub unknown: serde_json::Map<String, serde_json::Value>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Settings {
    /// Put a typed model name first among the recent ones (at most six are kept).
    pub fn remember_model(&mut self, model: &str) {
        let model = model.trim();
        if model.is_empty() {
            return;
        }
        self.recent_models.retain(|m| m != model);
        self.recent_models.insert(0, model.to_string());
        self.recent_models.truncate(6);
    }

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

    /// The permission mode a session runs in: the one it was set to, else the default.
    pub fn mode_for(&self, session: Option<&str>) -> String {
        session
            .and_then(|id| self.session_modes.get(id))
            .cloned()
            .unwrap_or_else(|| if self.default_permission_mode.is_empty() { "auto".into() } else { self.default_permission_mode.clone() })
    }

    /// Remember a session's permission mode (kept to the last few hundred sessions).
    pub fn set_mode(&mut self, session: &str, mode: &str) {
        self.session_modes.insert(session.to_string(), mode.to_string());
        while self.session_modes.len() > 500 {
            let first = self.session_modes.keys().next().cloned().unwrap_or_default();
            self.session_modes.remove(&first);
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
    fn settings_from_a_newer_build_survive_a_save() {
        let path = std::env::temp_dir().join(format!("keleuo-settings-unknown-{}.json", std::process::id()));
        std::fs::write(&path, r#"{"appearance":"dark","from_the_future":{"on":true}}"#).unwrap();
        let mut s = Settings::load(Some(path.clone()));
        assert_eq!(s.appearance, "dark");
        s.appearance = "light".into();
        s.save().unwrap();
        let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["appearance"], "light");
        assert_eq!(saved["from_the_future"]["on"], true);
        let _ = std::fs::remove_file(&path);
    }


    #[test]
    fn prefs_are_on_unless_turned_off() {
        // An older file has no prefs at all: everything on.
        let old: Settings = serde_json::from_str(r#"{"appearance": "dark"}"#).unwrap();
        assert_eq!(old.prefs, Prefs::default());
        assert!(old.prefs.reopen_sessions && old.prefs.paste_pills && old.prefs.command_changes);
        // One turned off keeps the others on.
        let one: Settings = serde_json::from_str(r#"{"prefs": {"turn_card": false}}"#).unwrap();
        assert!(!one.prefs.turn_card && one.prefs.markdown_marks && one.prefs.spell_check);
        // A font size out of range is brought back into it.
        let big: Settings = serde_json::from_str(r#"{"prefs": {"terminal_font_size": 90}}"#).unwrap();
        assert_eq!(big.prefs.terminal_font_size(), 24.0);
    }

    #[test]
    fn typed_models_are_remembered_most_recent_first() {
        let mut s = Settings::default();
        for m in ["a", "b", "a", " ", "c", "d", "e", "f", "g"] {
            s.remember_model(m);
        }
        assert_eq!(s.recent_models, ["g", "f", "e", "d", "c", "a"]);
    }

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
    fn notifications_are_on_unless_turned_off() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.notify, Notify { waiting: true, finished: true });
        let s: Settings = serde_json::from_str(r#"{"notify":{"finished":false}}"#).unwrap();
        assert_eq!(s.notify, Notify { waiting: true, finished: false });
    }

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
            SavedSession { number: 2, folder: "/p/a".into(), backend: "claude".into(), id: Some("abc".into()), ..Default::default() },
            SavedSession { number: 5, folder: "/p/b".into(), backend: "claude".into(), id: None, ..Default::default() },
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
