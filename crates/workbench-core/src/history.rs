//! Earlier Claude Code sessions of a folder, as Claude Code keeps them: one JSONL file per
//! session under `~/.claude/projects/<folder path, every character other than a letter or a
//! digit replaced by '-'>/<session id>.jsonl`.
//!
//! Read-only. The window lists them (title, last activity, message count) and, to resume one,
//! renders its history from the file before starting `claude --resume <id>`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::conversation::{Conversation, Item};
use crate::host::Host;
use crate::stream;
use crate::summary::one_line;

/// `~/.claude/projects` folder name for a project folder.
pub fn encode_project_dir(folder: &Path) -> String {
    folder
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

pub fn project_sessions_dir(claude_home: &Path, folder: &Path) -> PathBuf {
    claude_home
        .join("projects")
        .join(encode_project_dir(folder))
}

/// One earlier session.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionInfo {
    pub id: String,
    pub path: PathBuf,
    /// The first thing the person asked.
    pub title: String,
    /// ISO time of the last message.
    pub last_activity: Option<String>,
    /// User and assistant messages (not counting subagents).
    pub messages: usize,
    /// File modification time, seconds since the epoch, when known.
    pub modified: Option<u64>,
}

/// Title, message count and last activity of a session file's text.
pub fn summarize(text: &str) -> (String, usize, Option<String>) {
    let mut title = None;
    let mut messages = 0;
    let mut last = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let ty = v.get("type").and_then(Value::as_str).unwrap_or_default();
        if !matches!(ty, "user" | "assistant")
            || v.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        messages += 1;
        if let Some(t) = v.get("timestamp").and_then(Value::as_str) {
            last = Some(t.to_string());
        }
        if title.is_none() && ty == "user" && v.get("isMeta").and_then(Value::as_bool) != Some(true)
        {
            title = user_text(&v).map(|t| one_line(&t, 80));
        }
    }
    (
        title.unwrap_or_else(|| "(no messages)".into()),
        messages,
        last,
    )
}

/// The person's own text in a `user` line (not tool results, not command or system tags).
fn user_text(v: &Value) -> Option<String> {
    let content = v.get("message")?.get("content")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let t = text.trim();
    (!t.is_empty() && !t.starts_with('<')).then(|| t.to_string())
}

/// The sessions of `folder`, newest first, at most `max`.
pub fn list_sessions(
    host: &dyn Host,
    claude_home: &Path,
    folder: &Path,
    max: usize,
) -> Vec<SessionInfo> {
    let dir = project_sessions_dir(claude_home, folder);
    let Ok(entries) = host.read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<(PathBuf, Option<u64>)> = entries
        .into_iter()
        .filter(|e| !e.is_dir && e.name.ends_with(".jsonl"))
        .map(|e| {
            let p = dir.join(&e.name);
            let m = host.modified(&p);
            (p, m)
        })
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.1));
    files
        .into_iter()
        .take(max)
        .filter_map(|(path, modified)| {
            let text = host.read_to_string(&path).ok()?;
            let (title, messages, last_activity) = summarize(&text);
            let id = path.file_stem()?.to_string_lossy().to_string();
            (messages > 0).then_some(SessionInfo {
                id,
                path,
                title,
                last_activity,
                messages,
                modified,
            })
        })
        .collect()
}

/// Rebuild the chat of a session from its file.
pub fn load_conversation(text: &str, cwd: &str) -> Conversation {
    let mut chat = Conversation::new(cwd);
    chat.replaying = true;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if chat.session_id.is_none() {
            chat.session_id = v
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        match v.get("type").and_then(Value::as_str) {
            Some("user") => {
                let is_result = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(Value::as_array)
                    .is_some_and(|b| {
                        b.iter()
                            .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                    });
                if is_result {
                    chat.apply(stream::parse_value(&v));
                } else if v.get("isMeta").and_then(Value::as_bool) != Some(true)
                    && let Some(text) = user_text(&v)
                {
                    chat.items.push(Item::User(text));
                    chat.turn_agents_from = chat.agents.len();
                }
            }
            Some("assistant") => {
                chat.apply(stream::parse_value(&v));
            }
            _ => {}
        }
    }
    chat.busy = false;
    chat.status = None;
    chat.replaying = false;
    chat
}

/// Add a subagent's own transcript (`<session>/subagents/agent-<id>.jsonl`) to a rebuilt chat,
/// under the Agent tool call `tool_use_id` (from the agent's `.meta.json`).
pub fn attach_subagent(chat: &mut Conversation, tool_use_id: &str, text: &str) {
    chat.replaying = true;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let event = match v.get("type").and_then(Value::as_str) {
            Some("assistant") => match stream::parse_value(&v) {
                stream::Event::Assistant {
                    message_id,
                    model,
                    blocks,
                    ..
                } => stream::Event::Assistant {
                    message_id,
                    model,
                    blocks,
                    parent_tool_use_id: Some(tool_use_id.to_string()),
                },
                _ => continue,
            },
            Some("user") => match stream::parse_value(&v) {
                stream::Event::ToolResult {
                    tool_use_id: id,
                    content,
                    is_error,
                    structured,
                    ..
                } => stream::Event::ToolResult {
                    tool_use_id: id,
                    content,
                    is_error,
                    structured,
                    parent_tool_use_id: Some(tool_use_id.to_string()),
                },
                _ => continue,
            },
            _ => continue,
        };
        chat.apply(event);
    }
    chat.replaying = false;
}

/// Read a session's file and its subagents' transcripts and rebuild the chat.
pub fn load_session(host: &dyn Host, path: &Path, cwd: &str) -> std::io::Result<Conversation> {
    let mut chat = load_conversation(&host.read_to_string(path)?, cwd);
    let dir = path.with_extension("").join("subagents");
    if let Ok(entries) = host.read_dir(&dir) {
        for e in entries.iter().filter(|e| e.name.ends_with(".meta.json")) {
            let Ok(meta) = host.read_to_string(&dir.join(&e.name)) else {
                continue;
            };
            let Some(tool_use_id) = serde_json::from_str::<Value>(&meta).ok().and_then(|m| {
                m.get("toolUseId")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }) else {
                continue;
            };
            let transcript = dir.join(e.name.replace(".meta.json", ".jsonl"));
            if let Ok(text) = host.read_to_string(&transcript) {
                attach_subagent(&mut chat, &tool_use_id, &text);
            }
        }
    }
    Ok(chat)
}

/// A `claude` process running somewhere on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeProcess {
    pub pid: u32,
    pub ppid: u32,
    pub args: Vec<String>,
}

impl ClaudeProcess {
    /// The session id given on its command line (`--resume`, `-r`, `--session-id`).
    pub fn session_arg(&self) -> Option<&str> {
        self.args
            .windows(2)
            .find(|w| matches!(w[0].as_str(), "--resume" | "-r" | "--session-id"))
            .map(|w| w[1].as_str())
    }
}

/// `claude` processes from `ps -axo pid=,ppid=,command=` output.
pub fn parse_ps(output: &str) -> Vec<ClaudeProcess> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?.parse().ok()?;
            let ppid = parts.next()?.parse().ok()?;
            let args: Vec<String> = parts.map(str::to_string).collect();
            let program = args.first()?;
            let base = program.rsplit('/').next().unwrap_or(program);
            // The native binary is `claude`; an npm install runs `node .../claude`.
            let is_claude = base == "claude"
                || (base == "node"
                    && args
                        .get(1)
                        .is_some_and(|a| a.ends_with("/claude") || a.ends_with("cli.js")));
            is_claude.then_some(ClaudeProcess { pid, ppid, args })
        })
        .collect()
}

/// `claude` processes on this machine whose working folder is `folder`, except ours (those
/// whose parent is `own_pid`). Read-only: `ps` and `lsof`, nothing is signalled.
pub fn claude_processes_in(folder: &Path, own_pid: u32) -> Vec<ClaudeProcess> {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output()
    else {
        return Vec::new();
    };
    parse_ps(&String::from_utf8_lossy(&out.stdout))
        .into_iter()
        .filter(|p| p.ppid != own_pid && p.pid != own_pid)
        .filter(|p| process_cwd(p.pid).as_deref() == Some(folder))
        .collect()
}

/// The working folder of a process (`lsof` on macOS, `/proc` on Linux).
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    if cfg!(target_os = "linux") {
        return std::fs::read_link(format!("/proc/{pid}/cwd")).ok();
    }
    let out = std::process::Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix('n').map(PathBuf::from))
}

/// Whether a process still exists (asks `ps`; sends no signal).
pub fn process_alive(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.is_empty())
}

/// Which session each process is writing: the one on its command line, else the most
/// recently changed session not already claimed (a heuristic: Claude Code does not keep the
/// file open). Returns `(pid, session id)` pairs.
pub fn match_processes(
    processes: &[ClaudeProcess],
    sessions: &[SessionInfo],
) -> Vec<(u32, String)> {
    let mut out: Vec<(u32, String)> = Vec::new();
    let mut by_arg: Vec<&ClaudeProcess> = Vec::new();
    let mut rest: Vec<&ClaudeProcess> = Vec::new();
    for p in processes {
        match p.session_arg() {
            Some(id) if sessions.iter().any(|s| s.id == id) => by_arg.push(p),
            _ => rest.push(p),
        }
    }
    for p in by_arg {
        out.push((p.pid, p.session_arg().unwrap().to_string()));
    }
    let mut newest: Vec<&SessionInfo> = sessions.iter().collect();
    newest.sort_by_key(|s| std::cmp::Reverse(s.modified));
    for p in rest {
        if let Some(s) = newest
            .iter()
            .find(|s| !out.iter().any(|(_, id)| *id == s.id))
        {
            out.push((p.pid, s.id.clone()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
        )
        .unwrap()
    }

    #[test]
    fn encodes_folders_like_claude_code() {
        assert_eq!(
            encode_project_dir(Path::new("/Volumes/Work Disk/workspace/other-app")),
            "-Volumes-Work-Disk-workspace-other-app"
        );
        assert_eq!(
            encode_project_dir(Path::new("/private/tmp/a_b.c")),
            "-private-tmp-a-b-c"
        );
    }

    #[test]
    fn summarizes_a_session_file() {
        let (title, messages, last) = summarize(&fixture("session_history.jsonl"));
        assert!(
            title.starts_with("Launch two subagents in parallel"),
            "{title}"
        );
        assert_eq!(messages, 8);
        assert!(last.unwrap().starts_with("2026-10-11T"));
        assert_eq!(summarize("").0, "(no messages)");
    }

    #[test]
    fn rebuilds_the_chat_from_history() {
        let chat = load_conversation(&fixture("session_history.jsonl"), "/tmp/wb-proj");
        assert_eq!(
            chat.session_id.as_deref(),
            Some("ddb9dd2c-7823-44ae-b97b-6463fbdeddb5")
        );
        let kinds: Vec<&str> = chat
            .items
            .iter()
            .map(|i| match i {
                Item::User(_) => "user",
                Item::Assistant { .. } => "text",
                Item::Tool(_) => "tool",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["user", "tool", "tool", "text"]);
        // Both Agent calls became finished subagents.
        assert_eq!(chat.agents.len(), 2);
        assert!(chat.agents.iter().all(|a| !a.running()));
        assert!(!chat.busy);
    }

    #[test]
    fn attaches_a_subagent_transcript() {
        let mut chat = load_conversation(&fixture("session_history.jsonl"), "/tmp/wb-proj");
        let id = chat.agents[0].tool_use_id.clone();
        attach_subagent(&mut chat, &id, &fixture("subagent_transcript.jsonl"));
        let agent = chat.agent(&id).unwrap();
        let names: Vec<&str> = agent
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Tool(t) => Some(t.name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["Read"]);
        assert!(matches!(&agent.items[0], Item::Tool(t) if t.done()));
        assert!(
            agent
                .items
                .iter()
                .any(|i| matches!(i, Item::Assistant { .. }))
        );
    }

    #[test]
    fn finds_claude_processes_and_their_sessions() {
        let ps = "  101     1 /Users/u/.local/bin/claude --resume s2\n\
                    102     1 claude\n\
                    103   900 /usr/bin/node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js -p\n\
                    104     1 /bin/zsh -l\n\
                    105     1 /Applications/Claude.app/Contents/MacOS/Claude\n";
        let procs = parse_ps(ps);
        assert_eq!(
            procs.iter().map(|p| p.pid).collect::<Vec<_>>(),
            [101, 102, 103]
        );
        assert_eq!(procs[0].session_arg(), Some("s2"));
        let s = |id: &str, m: u64| SessionInfo {
            id: id.into(),
            path: PathBuf::new(),
            title: String::new(),
            last_activity: None,
            messages: 1,
            modified: Some(m),
        };
        let sessions = [s("s1", 10), s("s2", 30), s("s3", 20)];
        let matched = match_processes(&procs[..2], &sessions);
        assert_eq!(matched, [(101, "s2".to_string()), (102, "s3".to_string())]);
    }
}
