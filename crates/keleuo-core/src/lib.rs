//! Keleuo core: everything that does not need a window.
//!
//! - [`agent`]: the agent behind a session in neutral terms: events, the `Agent` and `Backend`
//!   traits, capabilities and the registry;
//! - [`backends`]: the agents themselves (Claude Code, and a scripted one for tests);
//! - [`conversation`]: the chat state built from agent events;
//! - [`skills`]: skills and similar files, grouped for the sidebar;
//! - [`frontmatter`]: the YAML header of a SKILL.md or agent file;
//! - [`files`]: the project file tree, respecting .gitignore;
//! - [`diff`]: file diffs of edit tool calls;
//! - [`settings`]: recent folders and the remembered layout;
//! - [`host`]: where the project lives (this machine or over SSH); every operation and every
//!   program an agent runs goes through it;
//! - [`tabs`]: automatic and user-given tab names;
//! - [`links`]: `path:line` references in text, resolved to project files;
//! - [`symbols`]: a quick definition index for go to definition and outlines;
//! - [`web`]: what an address typed in the Web panel means;
//! - [`compose`]: writing a message (lists that continue on Shift+Enter).

pub mod agent;
pub mod backends;
pub mod compose;
pub mod conversation;
pub mod diff;
pub mod files;
pub mod frontmatter;
pub mod images;
pub mod host;
pub mod links;
pub mod paste;
pub mod settings;
pub mod skills;
pub mod symbols;
pub mod tabs;
pub mod web;

/// An environment setting `KELEUO_<name>`, or one of the app's earlier names
/// (`KELEION_<name>`, `WORKBENCH_<name>`; accepted too).
pub fn env_var(name: &str) -> Result<String, std::env::VarError> {
    std::env::var(format!("KELEUO_{name}"))
        .or_else(|_| std::env::var(format!("KELEION_{name}")))
        .or_else(|_| std::env::var(format!("WORKBENCH_{name}")))
}

/// The same, as an `OsString`.
pub fn env_var_os(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(format!("KELEUO_{name}"))
        .or_else(|| std::env::var_os(format!("KELEION_{name}")))
        .or_else(|| std::env::var_os(format!("WORKBENCH_{name}")))
}
