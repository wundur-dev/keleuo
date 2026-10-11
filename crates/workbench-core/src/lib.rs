//! Workbench core: everything that does not need a window.
//!
//! - [`skills`]: skills, agents, commands and CLAUDE.md files relevant to a project;
//! - [`frontmatter`]: the YAML header of a SKILL.md or agent file;
//! - [`files`]: the project file tree, respecting .gitignore;
//! - [`stream`]: the Claude Code `stream-json` protocol, line by line;
//! - [`conversation`]: the chat state built from that stream;
//! - [`diff`]: file diffs pulled out of Edit, MultiEdit and Write tool calls;
//! - [`summary`]: the one-line summary on a tool card;
//! - [`agent`]: the agent behind a chat session (only Claude Code so far);
//! - [`claude`]: the `claude` subprocess itself;
//! - [`settings`]: recent folders and the remembered layout;
//! - [`host`]: where the project lives (this machine or over SSH); every operation goes through it;
//! - [`tabs`]: automatic and user-given tab names;
//! - [`links`]: `path:line` references in text, resolved to project files;
//! - [`symbols`]: a quick definition index for go to definition and outlines.

pub mod agent;
pub mod claude;
pub mod conversation;
pub mod diff;
pub mod files;
pub mod frontmatter;
pub mod host;
pub mod links;
pub mod settings;
pub mod skills;
pub mod stream;
pub mod summary;
pub mod symbols;
pub mod tabs;
