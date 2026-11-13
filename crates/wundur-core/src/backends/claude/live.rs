//! A running Bash command's output, live. The interface writes each command's output to a
//! file as it runs (`/tmp/claude-<uid>/<project>/<session>/tasks/<id>.output`) and puts it in
//! the session file only when the command ends. The file names do not say which tool call
//! they belong to, so a new file goes to the oldest running Bash call still without one.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::agent::Event;
use crate::host::Host;

/// The latest part of the output kept (and shown).
const KEEP: usize = 8 * 1024;

pub struct LiveOutputs {
    dir: PathBuf,
    /// Running Bash calls without an output file yet, oldest first, with when they were seen.
    waiting: Vec<(String, SystemTime)>,
    /// Output files followed: file name to (tool call, bytes read, text so far).
    files: HashMap<String, (String, u64, String)>,
    /// Files not to take: there before following began, a subagent's, or taken already.
    seen: HashSet<String>,
}

impl LiveOutputs {
    /// For the session file at `session` (`~/.claude/projects/<project>/<id>.jsonl`), on this
    /// machine only.
    pub fn new(host: &dyn Host, session: &Path) -> Option<Self> {
        if host.is_remote() {
            return None;
        }
        let dir = tasks_dir(session)?;
        let seen = host.read_dir(&dir).map(|e| e.into_iter().map(|e| e.name).collect()).unwrap_or_default();
        Some(Self { dir, waiting: Vec::new(), files: HashMap::new(), seen })
    }

    /// Note Bash calls starting and tool calls ending.
    pub fn observe(&mut self, event: &Event) {
        match event {
            Event::ToolCall { id, name, .. } if name == "Bash" => {
                let known = self.waiting.iter().any(|(w, _)| w == id) || self.files.values().any(|(t, ..)| t == id);
                if !known {
                    self.waiting.push((id.clone(), SystemTime::now()));
                }
            }
            Event::ToolResult { id, .. } => {
                self.waiting.retain(|(w, _)| w != id);
                self.files.retain(|_, (t, ..)| t != id);
            }
            Event::Interrupted | Event::TurnEnded { .. } => {
                self.waiting.clear();
                self.files.clear();
            }
            _ => {}
        }
    }

    /// Whether anything is being waited for or followed.
    pub fn active(&self) -> bool {
        !self.waiting.is_empty() || !self.files.is_empty()
    }

    /// New output since the last call, as events.
    pub fn poll(&mut self, host: &dyn Host) -> Vec<Event> {
        if !self.active() {
            return Vec::new();
        }
        if !self.waiting.is_empty()
            && let Ok(entries) = host.read_dir(&self.dir)
        {
            let mut new: Vec<(SystemTime, String)> = Vec::new();
            for e in entries {
                if !e.name.ends_with(".output") || self.seen.contains(&e.name) {
                    continue;
                }
                // Subagents' transcripts are here too (`a…`); commands are `b…`.
                if !e.name.starts_with('b') {
                    self.seen.insert(e.name);
                    continue;
                }
                let secs = host.modified(&self.dir.join(&e.name)).unwrap_or(0);
                new.push((SystemTime::UNIX_EPOCH + Duration::from_secs(secs), e.name));
            }
            new.sort();
            for (modified, name) in new {
                let Some(ix) = self.waiting.iter().position(|(_, since)| modified + Duration::from_secs(2) >= *since) else {
                    continue;
                };
                let (tool, _) = self.waiting.remove(ix);
                self.seen.insert(name.clone());
                self.files.insert(name, (tool, 0, String::new()));
                if self.waiting.is_empty() {
                    break;
                }
            }
        }
        let mut out = Vec::new();
        for (name, (tool, offset, text)) in &mut self.files {
            let Ok(bytes) = host.read_from(&self.dir.join(name.as_str()), *offset) else { continue };
            if bytes.is_empty() {
                continue;
            }
            *offset += bytes.len() as u64;
            text.push_str(&String::from_utf8_lossy(&bytes));
            if text.len() > KEEP * 2 {
                let mut cut = text.len() - KEEP;
                while !text.is_char_boundary(cut) {
                    cut += 1;
                }
                text.drain(..cut);
            }
            out.push(Event::ToolOutput { id: tool.clone(), text: plain(text) });
        }
        out
    }
}

/// `/tmp/claude-<uid>/<project>/<session>/tasks` for a session file. Not known on Windows (no
/// running commands' output there yet).
#[cfg(unix)]
fn tasks_dir(session: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata(session).ok()?.uid();
    let project = session.parent()?.file_name()?;
    let id = session.file_stem()?;
    Some(PathBuf::from(format!("/tmp/claude-{uid}")).join(project).join(id).join("tasks"))
}

#[cfg(not(unix))]
fn tasks_dir(_session: &Path) -> Option<PathBuf> {
    None
}

/// Output as a terminal would leave it on screen: colour codes dropped, and a line redrawn
/// with carriage returns (progress bars) kept as its last state.
pub fn plain(text: &str) -> String {
    let mut no_escapes = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                // Parameters, then one final byte in @..~.
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        no_escapes.push(c);
    }
    no_escapes
        .split('\n')
        .map(|line| line.trim_end_matches('\r').rsplit('\r').next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn bash(id: &str) -> Event {
        Event::ToolCall { id: id.into(), name: "Bash".into(), summary: String::new(), input: Value::Null, diff: None, parent: None }
    }

    #[test]
    fn follows_a_new_command_file_for_the_running_call() {
        let root = std::env::temp_dir().join(format!("kl-live-{}", std::process::id()));
        let dir = root.join("tasks");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bold.output"), "from before").unwrap();
        let host = crate::host::LocalHost;
        let seen = host.read_dir(&dir).unwrap().into_iter().map(|e| e.name).collect();
        let mut live = LiveOutputs { dir: dir.clone(), waiting: Vec::new(), files: HashMap::new(), seen };
        assert!(live.poll(&host).is_empty(), "nothing running");
        live.observe(&bash("t1"));
        std::fs::write(dir.join("a123.output"), "a subagent").unwrap();
        std::fs::write(dir.join("bnew.output"), "line 1\n").unwrap();
        let got = live.poll(&host);
        assert_eq!(got, vec![Event::ToolOutput { id: "t1".into(), text: "line 1\n".into() }]);
        std::fs::write(dir.join("bnew.output"), "line 1\nline 2\n").unwrap();
        assert_eq!(live.poll(&host), vec![Event::ToolOutput { id: "t1".into(), text: "line 1\nline 2\n".into() }]);
        assert!(live.poll(&host).is_empty(), "no change, no event");
        live.observe(&Event::ToolResult { id: "t1".into(), output: String::new(), is_error: false, diff: None, file_changed: None, parent: None });
        assert!(!live.active());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn output_reads_as_on_screen() {
        assert_eq!(plain("\u{1b}[32mok\u{1b}[0m\n10%\r50%\r100%\n"), "ok\n100%\n");
    }
}
