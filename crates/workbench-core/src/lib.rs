//! Workbench core: everything that does not need a window.
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
//! - [`symbols`]: a quick definition index for go to definition and outlines.

pub mod agent;
pub mod backends;
pub mod conversation;
pub mod diff;
pub mod files;
pub mod frontmatter;
pub mod host;
pub mod links;
pub mod settings;
pub mod skills;
pub mod symbols;
pub mod tabs;
