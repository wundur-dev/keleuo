//! Earlier Claude Code sessions of a folder, as Claude Code keeps them: one JSONL file per
//! session under `~/.claude/projects/<folder path, every character other than a letter or a
//! digit replaced by '-'>/<session id>.jsonl`.
//!
//! Read-only. The window lists them (title, last activity, message count) and, to resume one,
//! renders its history from the file before starting `claude --resume <id>`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::map::Mapper;
use super::pricing::Tally;
use super::stream;
pub use crate::agent::SessionInfo;
use crate::agent::Event;
use crate::conversation::{Conversation, one_line};
use crate::host::Host;

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

/// Title, message count and last activity of a session file's text.
pub fn summarize(text: &str) -> (String, usize, Option<String>) {
    let mut s = Summary::default();
    for line in text.lines() {
        s.add(line);
    }
    s.finish()
}

/// The same, read from the file as a stream (session files can be very large).
pub fn summarize_file(host: &dyn Host, path: &Path) -> std::io::Result<(String, usize, Option<String>)> {
    let mut s = Summary::default();
    host.for_each_line(path, 0, None, &mut |_, line| {
        s.add(line);
        true
    })?;
    Ok(s.finish())
}

#[derive(Default)]
struct Summary {
    title: Option<String>,
    messages: usize,
    last: Option<String>,
}

impl Summary {
    fn add(&mut self, line: &str) {
        // Cheap test first: skip lines that cannot be a message without parsing them.
        if !line.contains("\"user\"") && !line.contains("\"assistant\"") {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        let ty = v.get("type").and_then(Value::as_str).unwrap_or_default();
        if !matches!(ty, "user" | "assistant") || v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return;
        }
        self.messages += 1;
        if let Some(t) = v.get("timestamp").and_then(Value::as_str) {
            self.last = Some(t.to_string());
        }
        if self.title.is_none()
            && ty == "user"
            && v.get("isMeta").and_then(Value::as_bool) != Some(true)
            && v.get("isCompactSummary").and_then(Value::as_bool) != Some(true)
        {
            self.title = user_text(&v).map(|t| one_line(&crate::paste::title_text(&t), 80));
        }
    }

    fn finish(self) -> (String, usize, Option<String>) {
        (self.title.unwrap_or_else(|| "(no messages)".into()), self.messages, self.last)
    }
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

/// The images in a `user` line, stored in the session's image folder.
fn user_images(v: &Value) -> Vec<PathBuf> {
    let Some(blocks) = v.get("message").and_then(|m| m.get("content")).and_then(Value::as_array) else { return Vec::new() };
    let session = v.get("sessionId").and_then(Value::as_str);
    let Some(dir) = crate::images::session_dir(session) else { return Vec::new() };
    blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("image"))
        .filter_map(|b| {
            let source = b.get("source")?;
            let data = crate::images::base64(source.get("data")?.as_str()?)?;
            let ext = crate::images::extension(source.get("media_type").and_then(Value::as_str).unwrap_or("image/png"));
            crate::images::store(&dir, &data, ext).ok()
        })
        .collect()
}

/// Whether a session file exists and holds a message (read until the first one).
pub fn has_messages(host: &dyn Host, path: &Path) -> bool {
    has_messages_from(host, path, 0)
}

/// Whether the file holds a message from byte `from` on (a turn written since then).
pub fn has_messages_from(host: &dyn Host, path: &Path, from: u64) -> bool {
    let mut found = false;
    let _ = host.for_each_line(path, from, None, &mut |_, line| {
        if (line.contains("\"type\":\"user\"") || line.contains("\"type\":\"assistant\""))
            && serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|v| v.get("type").and_then(Value::as_str).map(|t| t == "user" || t == "assistant"))
                == Some(true)
        {
            found = true;
            return false;
        }
        true
    });
    found
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
            let (title, messages, last_activity) = summarize_file(host, &path).ok()?;
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

/// The folder `old` was renamed to `new`: its project folder under `claude_home` (sessions,
/// memory) moves to the new name. When the new name has one already (a session was started
/// there first), what it lacks is moved in, folder by folder, and the rest stays where it was.
pub fn move_project(claude_home: &Path, old: &Path, new: &Path) -> std::io::Result<()> {
    let (from, to) = (project_sessions_dir(claude_home, old), project_sessions_dir(claude_home, new));
    if from == to || !from.is_dir() {
        return Ok(());
    }
    if !to.exists() {
        return std::fs::rename(&from, &to);
    }
    merge_into(&from, &to)?;
    // Gone if everything moved; kept (with what could not) otherwise.
    let _ = std::fs::remove_dir(&from);
    Ok(())
}

fn merge_into(from: &Path, to: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(from)?.flatten() {
        let target = to.join(entry.file_name());
        if !target.exists() {
            std::fs::rename(entry.path(), &target)?;
        } else if entry.path().is_file() && std::fs::read(entry.path()).ok() == std::fs::read(&target).ok() {
            // The same file in both: one copy is enough.
            let _ = std::fs::remove_file(entry.path());
        } else if entry.path().is_dir() && target.is_dir() {
            merge_into(&entry.path(), &target)?;
            let _ = std::fs::remove_dir(entry.path());
        }
    }
    Ok(())
}

/// How many sessions `folder` has and when the newest changed, without reading them.
pub fn session_count(host: &dyn Host, claude_home: &Path, folder: &Path) -> (usize, Option<u64>) {
    let dir = project_sessions_dir(claude_home, folder);
    let Ok(entries) = host.read_dir(&dir) else {
        return (0, None);
    };
    let files: Vec<PathBuf> = entries
        .into_iter()
        .filter(|e| !e.is_dir && e.name.ends_with(".jsonl"))
        .map(|e| dir.join(e.name))
        .collect();
    let newest = files.iter().filter_map(|p| host.modified(p)).max();
    (files.len(), newest)
}

/// The events one line of a session file stands for (sidechain lines belong to subagents'
/// own files and are skipped).
fn line_events(v: &Value, mapper: &mut Mapper) -> Vec<Event> {
    if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return Vec::new();
    }
    match v.get("type").and_then(Value::as_str) {
        Some("user") => {
            let is_result = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_array)
                .is_some_and(|b| b.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")));
            if is_result {
                mapper.map(stream::parse_value(v))
            } else if super::messages::is_interruption(v) {
                vec![Event::Interrupted]
            } else if let Some((from, text)) = super::messages::agent_message(v) {
                vec![Event::AgentMessage { from, text }]
            } else if let Some((id, failed)) = super::messages::task_ended(v) {
                // Background shell commands notify too: only subagents end here.
                if let Some(id) = mapper.agent_call(&id) {
                    vec![Event::SubagentEnded { id, failed, summary: None, tokens: None, duration_ms: None }]
                } else {
                    Vec::new()
                }
            } else if v.get("isCompactSummary").and_then(Value::as_bool) == Some(true) {
                // Shown as the compaction divider, not as something the person wrote.
                Vec::new()
            } else if v.get("isMeta").and_then(Value::as_bool) != Some(true) {
                let mut out: Vec<Event> = user_text(v).map(Event::UserText).into_iter().collect();
                let images = user_images(v);
                if !images.is_empty() {
                    out.push(Event::UserImages(images));
                }
                out
            } else {
                Vec::new()
            }
        }
        Some("assistant") => mapper.map(stream::parse_value(v)),
        // Delivered while a turn ran (a message the person sent, another agent's report, a
        // task's end): recorded as a queued attachment, read as the user line it stands for.
        Some("attachment") => {
            let a = v.get("attachment");
            let prompt = a
                .filter(|a| a.get("type").and_then(Value::as_str) == Some("queued_command"))
                .and_then(|a| a.get("prompt"))
                .and_then(Value::as_str);
            match prompt {
                Some(prompt) => {
                    let mut line = serde_json::json!({"type": "user", "message": {"role": "user", "content": prompt}});
                    if let Some(origin) = a.and_then(|a| a.get("origin")) {
                        line["origin"] = origin.clone();
                    }
                    line_events(&line, mapper)
                }
                None => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// A tool result in a history longer than this keeps only its start (the card shows the first
/// lines anyway), so a long session costs memory for what can be seen.
const RESULT_KEEP: usize = 16 * 1024;

fn trim_results(items: &mut [crate::conversation::Item]) {
    for item in items {
        if let crate::conversation::Item::Tool(t) = item
            && let Some(r) = t.result.as_mut()
            && r.len() > RESULT_KEEP
        {
            let mut cut = RESULT_KEEP;
            while !r.is_char_boundary(cut) {
                cut -= 1;
            }
            let more = (r.len() - cut) / 1024;
            r.truncate(cut);
            r.push_str(&format!("\n... ({more} KB more in the session file)"));
        }
    }
}

/// Whether a line is a compaction boundary.
fn is_boundary(v: &Value) -> bool {
    v.get("type").and_then(Value::as_str) == Some("system") && v.get("subtype").and_then(Value::as_str) == Some("compact_boundary")
}

/// A stretch of the file before a compaction, and the compaction that ended it.
struct Segment {
    from: u64,
    to: u64,
    messages: usize,
}

/// Rebuild a session's chat reading its file as a stream, never whole. Only the live context
/// (after the last compaction) becomes items now; each stretch before a compaction becomes an
/// [`Item::Earlier`] placeholder (read by [`load_segment`] when it is scrolled to) followed by
/// an [`Item::Compacted`] divider with the summary. The cost estimate and the token counts
/// come from the whole file.
pub fn load_streaming(host: &dyn Host, path: &Path, cwd: &str) -> std::io::Result<Conversation> {
    use crate::conversation::Item;
    let fresh = || {
        let mut c = Conversation::new(cwd);
        c.replaying = true;
        c
    };
    let mut chat = fresh();
    let mut mapper = Mapper::new(cwd);
    mapper.history = true;
    let mut tally = Tally::default();
    let mut cost = 0.0;
    let mut session_id: Option<String> = None;
    let mut last_turn: Option<Event> = None;
    let mut segments: Vec<Segment> = Vec::new();
    let mut dividers: Vec<Item> = Vec::new();
    let (mut seg_from, mut seg_messages) = (0u64, 0usize);
    host.for_each_line(path, 0, None, &mut |offset, line| {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return true };
        if session_id.is_none() {
            session_id = v.get("sessionId").and_then(Value::as_str).map(str::to_string);
        }
        if is_boundary(&v) {
            segments.push(Segment { from: seg_from, to: offset, messages: seg_messages });
            let meta = v.get("compactMetadata");
            dividers.push(Item::Compacted {
                summary: String::new(),
                trigger: meta.and_then(|m| m.get("trigger")).and_then(Value::as_str).map(str::to_string),
                tokens: meta.and_then(|m| m.get("preTokens")).and_then(Value::as_u64),
            });
            seg_from = offset;
            seg_messages = 0;
            // What came before is not kept: only the live context is built now.
            chat = fresh();
            mapper = Mapper::new(cwd);
            mapper.history = true;
            return true;
        }
        if v.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
            && let Some(Item::Compacted { summary, .. }) = dividers.last_mut()
            && summary.is_empty()
        {
            *summary = user_text(&v).unwrap_or_default();
        }
        let sidechain = v.get("isSidechain").and_then(Value::as_bool) == Some(true);
        if !sidechain && matches!(v.get("type").and_then(Value::as_str), Some("user" | "assistant")) {
            seg_messages += 1;
        }
        chat.apply_all(line_events(&v, &mut mapper));
        if let Some(end) = turn_end(&v) {
            last_turn = Some(end.clone());
            chat.apply(end);
        }
        if !sidechain {
            cost += tally.add(&v);
        }
        true
    })?;
    let mut head: Vec<Item> = Vec::new();
    for (seg, divider) in segments.into_iter().zip(dividers) {
        if seg.messages > 0 {
            head.push(Item::Earlier { from: seg.from, to: seg.to, messages: seg.messages });
        }
        head.push(divider);
    }
    if !head.is_empty() {
        head.append(&mut chat.items);
        chat.items = head;
    }
    if chat.last_turn_tokens.is_none()
        && let Some(end) = last_turn
    {
        chat.apply(end);
    }
    if cost > 0.0 {
        chat.apply(Event::CostEstimate { usd: cost - chat.total_cost_usd.min(cost) });
    }
    chat.session_id = session_id;
    chat.busy = false;
    chat.status = None;
    chat.replaying = false;
    trim_results(&mut chat.items);
    Ok(chat)
}

/// The items of the stretch `from..to` of a session file (an [`Item::Earlier`]), read as a
/// stream; the compaction line and its summary that open it are left out (the divider shows
/// them).
pub fn load_segment(host: &dyn Host, path: &Path, cwd: &str, from: u64, to: u64) -> std::io::Result<Vec<crate::conversation::Item>> {
    let mut chat = Conversation::new(cwd);
    chat.replaying = true;
    let mut mapper = Mapper::new(cwd);
    mapper.history = true;
    host.for_each_line(path, from, Some(to), &mut |_, line| {
        if let Ok(v) = serde_json::from_str::<Value>(line)
            && !is_boundary(&v)
        {
            chat.apply_all(line_events(&v, &mut mapper));
        }
        true
    })?;
    chat.finish_replay();
    trim_results(&mut chat.items);
    Ok(chat.items)
}

/// Rebuild the chat of a session from its file.
pub fn load_conversation(text: &str, cwd: &str) -> Conversation {
    let mut chat = Conversation::new(cwd);
    chat.replaying = true;
    let mut mapper = Mapper::new(cwd);
    mapper.history = true;
    let mut tally = Tally::default();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if chat.session_id.is_none() {
            chat.session_id = v.get("sessionId").and_then(Value::as_str).map(str::to_string);
        }
        chat.apply_all(line_events(&v, &mut mapper));
        if let Some(end) = turn_end(&v) {
            // The token counts of each finished turn (the last one gives the context in use).
            chat.apply(end);
        }
        if v.get("isSidechain").and_then(Value::as_bool) != Some(true) {
            let usd = tally.add(&v);
            if usd > 0.0 {
                chat.apply(Event::CostEstimate { usd });
            }
        }
    }
    chat.busy = false;
    chat.status = None;
    chat.replaying = false;
    chat
}

/// Add a subagent's own transcript (`<session>/subagents/agent-<id>.jsonl`) to a rebuilt chat,
/// under the Agent tool call `tool_use_id` (from the agent's `.meta.json`). A call the chat does
/// not hold (it came before a compaction, so only its stretch's placeholder is loaded) is left
/// out: its agent would show without a name and never finish.
pub fn attach_subagent(chat: &mut Conversation, tool_use_id: &str, text: &str) {
    if chat.agent(tool_use_id).is_none() {
        return;
    }
    chat.replaying = true;
    let mut mapper = Mapper::new(chat.cwd.clone());
    let mut tally = Tally::default();
    for line in text.lines() {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            chat.apply_all(subagent_line_events(&v, tool_use_id, &mut mapper));
            let usd = tally.add(&v);
            if usd > 0.0 {
                chat.apply(Event::CostEstimate { usd });
            }
        }
    }
    chat.replaying = false;
}

/// The events of one line of a subagent's transcript, under the tool call that started it.
fn subagent_line_events(v: &Value, tool_use_id: &str, mapper: &mut Mapper) -> Vec<Event> {
    let parent = Some(tool_use_id.to_string());
    let raw = match (v.get("type").and_then(Value::as_str), stream::parse_value(v)) {
        (Some("assistant"), stream::Event::Assistant { message_id, model, blocks, .. }) => {
            stream::Event::Assistant { message_id, model, blocks, parent_tool_use_id: parent, context_tokens: None, error: None }
        }
        (Some("user"), stream::Event::ToolResult { tool_use_id: id, content, is_error, structured, images, .. }) => {
            stream::Event::ToolResult { tool_use_id: id, content, is_error, structured, parent_tool_use_id: parent, images }
        }
        _ => return Vec::new(),
    };
    mapper.map(raw)
}

/// Follow a session file being written by Claude Code in a terminal: every complete line added
/// after now (and to its subagents' transcripts) is mapped and sent to `sink`, until `stop`.
/// An assistant message that ends its turn also ends the turn here, with its token counts, and
/// each new message's cost is estimated from its usage. Only what was added is read, when it
/// was added: on this machine the folder's change notices wake the follower; on another (over
/// ssh, where nothing tells) it looks every 300 ms while the file grows and less often, up to
/// every 2 s, while it does not.
pub fn follow(host: &dyn Host, path: &Path, cwd: &str, sink: &crate::agent::Sink, stop: &std::sync::atomic::AtomicBool) {
    use crate::agent::Message;
    use std::collections::HashMap;
    use std::sync::atomic::Ordering;

    struct Tail {
        offset: u64,
        /// The start of a line not yet complete.
        partial: Vec<u8>,
        mapper: Mapper,
        tally: Tally,
    }
    impl Tail {
        fn new(offset: u64, cwd: &str) -> Self {
            Self { offset, partial: Vec::new(), mapper: Mapper::new(cwd), tally: Tally::default() }
        }
        /// New complete lines, read from where the last read stopped.
        fn lines(&mut self, host: &dyn Host, p: &Path) -> Vec<Value> {
            let Ok(len) = host.len(p) else { return Vec::new() };
            if len < self.offset {
                // Rewritten: start over.
                self.offset = 0;
                self.partial.clear();
            }
            if len == self.offset {
                return Vec::new();
            }
            let Ok(bytes) = host.read_from(p, self.offset) else { return Vec::new() };
            self.offset += bytes.len() as u64;
            self.partial.extend_from_slice(&bytes);
            let Some(end) = self.partial.iter().rposition(|b| *b == b'\n') else { return Vec::new() };
            let rest = self.partial.split_off(end + 1);
            let chunk = std::mem::replace(&mut self.partial, rest);
            String::from_utf8_lossy(&chunk).lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
        }
    }

    let size = |p: &Path| host.len(p).unwrap_or(0);
    let mut main = Tail::new(size(path), cwd);
    main.mapper.history = true;
    let agents_dir = path.with_extension("").join("subagents");
    // Subagent transcripts: file name to (tail, the tool call that started it).
    let mut agents: HashMap<String, (Tail, Option<String>)> = HashMap::new();
    if let Ok(entries) = host.read_dir(&agents_dir) {
        for e in entries.iter().filter(|e| e.name.ends_with(".jsonl")) {
            agents.insert(e.name.clone(), (Tail::new(size(&agents_dir.join(&e.name)), cwd), None));
        }
    }
    let mut live = super::live::LiveOutputs::new(host, path);
    // Woken by the folder's change notices where the host can watch (the session file and its
    // subagents' transcripts are in it).
    let (woken, wake) = std::sync::mpsc::channel::<()>();
    let watch = path
        .parent()
        .and_then(|dir| host.watch(dir, std::sync::Arc::new(move |_| { let _ = woken.send(()); })));
    let mut round = 0u64;
    let mut quiet = 0u32;
    while !stop.load(Ordering::Relaxed) {
        let mut grew = false;
        for v in main.lines(host, path) {
            grew = true;
            for e in line_events(&v, &mut main.mapper) {
                if let Some(l) = live.as_mut() {
                    l.observe(&e);
                }
                sink(Message::Event(e));
            }
            if v.get("isSidechain").and_then(Value::as_bool) != Some(true) {
                let usd = main.tally.add(&v);
                if usd > 0.0 {
                    sink(Message::Event(Event::CostEstimate { usd }));
                }
            }
            if let Some(end) = turn_end(&v) {
                sink(Message::Event(end));
            }
        }
        // Subagents: a directory listing per look (one look in three when polling).
        if (watch.is_some() || round.is_multiple_of(3))
            && let Ok(entries) = host.read_dir(&agents_dir)
        {
            for e in entries.iter().filter(|e| e.name.ends_with(".jsonl")) {
                let entry = agents.entry(e.name.clone()).or_insert_with(|| (Tail::new(0, cwd), None));
                if entry.1.is_none() {
                    let meta = agents_dir.join(e.name.replace(".jsonl", ".meta.json"));
                    entry.1 = host
                        .read_to_string(&meta)
                        .ok()
                        .and_then(|m| serde_json::from_str::<Value>(&m).ok())
                        .and_then(|m| m.get("toolUseId").and_then(Value::as_str).map(str::to_string));
                }
                let Some(parent) = entry.1.clone() else { continue };
                for v in entry.0.lines(host, &agents_dir.join(&e.name)) {
                    grew = true;
                    for ev in subagent_line_events(&v, &parent, &mut entry.0.mapper) {
                        if let Some(l) = live.as_mut() {
                            l.observe(&ev);
                        }
                        sink(Message::Event(ev));
                    }
                    let usd = entry.0.tally.add(&v);
                    if usd > 0.0 {
                        sink(Message::Event(Event::CostEstimate { usd }));
                    }
                }
            }
        }
        // Running commands' output (written elsewhere, not watched).
        let running = live.as_ref().is_some_and(|l| l.active());
        if let Some(l) = live.as_mut() {
            for e in l.poll(host) {
                sink(Message::Event(e));
            }
        }
        round += 1;
        if watch.is_some() {
            // Until the folder changes: soon while a command runs (its output is elsewhere),
            // else a slow look in case a notice was missed.
            let wait = std::time::Duration::from_millis(if running { 200 } else { 2000 });
            if wake.recv_timeout(wait).is_ok() {
                while wake.try_recv().is_ok() {}
            }
        } else {
            // Over ssh each look is a round trip: every 300 ms while the file grows, backing off
            // to every 2 s while it does not.
            quiet = if grew { 0 } else { quiet.saturating_add(1) };
            let ms = (300u64 << quiet.min(3)).min(2000);
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
    }
}

/// An assistant message that ends the turn (`stop_reason` `end_turn`), as a turn end with its
/// token counts. The terminal interface does not write the cost to the file.
fn turn_end(v: &Value) -> Option<Event> {
    if v.get("isSidechain").and_then(Value::as_bool) == Some(true) || v.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let message = v.get("message")?;
    if message.get("stop_reason").and_then(Value::as_str) != Some("end_turn") {
        return None;
    }
    let usage = message.get("usage");
    let n = |k: &str| usage.and_then(|u| u.get(k)).and_then(Value::as_u64).unwrap_or(0);
    Some(Event::TurnEnded {
        error: None,
        cost_total_usd: None,
        input_tokens: n("input_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens"),
        output_tokens: n("output_tokens"),
        duration_ms: None,
        session_id: v.get("sessionId").and_then(Value::as_str).map(str::to_string),
    })
}

/// Read a session's file and its subagents' transcripts and rebuild the chat.
pub fn load_session(host: &dyn Host, path: &Path, cwd: &str) -> std::io::Result<Conversation> {
    let mut chat = load_streaming(host, path, cwd)?;
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
            if chat.agent(&tool_use_id).is_none() {
                continue;
            }
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
            // The words before the first option: the program, maybe split where its path has a
            // space (`ps` joins argv with spaces), maybe `node` and a script.
            let program: Vec<&str> = args.iter().map(String::as_str).take_while(|a| !a.starts_with('-')).take(4).collect();
            let base = |a: &str| a.rsplit('/').next().unwrap_or(a).to_string();
            // The native binary is `claude`; an npm install runs `node .../claude` or `cli.js`.
            let is_claude = program.iter().any(|a| base(a) == "claude")
                || (program.first().is_some_and(|a| base(a) == "node") && program.iter().any(|a| a.ends_with("cli.js")));
            is_claude.then_some(ClaudeProcess { pid, ppid, args })
        })
        .collect()
}

/// Every `claude` process on this machine except this window's (children of `own_pid`), with
/// whole command lines (`ps -ww`: never cut at a column). Read-only.
pub fn other_claude_processes(own_pid: u32) -> Vec<ClaudeProcess> {
    let Ok(out) = crate::platform::command("ps").args(["-axww", "-o", "pid=,ppid=,args="]).output() else {
        return Vec::new();
    };
    parse_ps(&String::from_utf8_lossy(&out.stdout))
        .into_iter()
        .filter(|p| p.ppid != own_pid && p.pid != own_pid)
        .collect()
}

/// What is known about a folder's sessions and the agent processes on the machine.
pub struct Seen<'a> {
    pub processes: &'a [ClaudeProcess],
    /// A process's working folder (`None`: could not tell).
    pub cwd_of: &'a dyn Fn(u32) -> Option<PathBuf>,
    pub folder: &'a Path,
    /// Whether session `id` belongs to this folder.
    pub has_session: &'a dyn Fn(&str) -> bool,
    pub sessions: &'a [SessionInfo],
    /// Seconds since each session file last changed.
    pub ages: &'a [(String, u64)],
    /// This window's own sessions.
    pub own: &'a [String],
}

/// A session file changed this recently may be open somewhere even when no process is found.
pub const RECENT_WRITE_SECS: u64 = 10;

/// The folder's sessions that are, or may be, open outside this window. A session counts as
/// open when a process names it on its command line (wherever that process runs, so a failed
/// folder lookup cannot hide it); when a process without one works in the folder (the newest
/// unclaimed session is taken as its); or when its file changed in the last few seconds.
pub fn live_sessions(seen: &Seen) -> Vec<crate::agent::LiveSession> {
    use crate::agent::{LiveReason, LiveSession};
    let mut out: Vec<LiveSession> = Vec::new();
    let claimed = |out: &Vec<LiveSession>, id: &str| out.iter().any(|l| l.id == id) || seen.own.iter().any(|o| o == id);
    for p in seen.processes {
        if let Some(id) = p.session_arg()
            && (seen.has_session)(id)
            && !claimed(&out, id)
        {
            out.push(LiveSession { id: id.to_string(), pid: Some(p.pid), reason: LiveReason::CommandLine });
        }
    }
    let mut newest: Vec<&SessionInfo> = seen.sessions.iter().collect();
    newest.sort_by_key(|s| std::cmp::Reverse(seen.ages.iter().find(|(id, _)| *id == s.id).map(|(_, a)| u64::MAX - a).or(s.modified)));
    for p in seen.processes.iter().filter(|p| p.session_arg().is_none()) {
        if (seen.cwd_of)(p.pid).as_deref() != Some(seen.folder) {
            continue;
        }
        if let Some(s) = newest.iter().find(|s| !claimed(&out, &s.id)) {
            out.push(LiveSession { id: s.id.clone(), pid: Some(p.pid), reason: LiveReason::Folder });
        }
    }
    for (id, age) in seen.ages {
        if *age <= RECENT_WRITE_SECS && !claimed(&out, id) {
            out.push(LiveSession { id: id.clone(), pid: None, reason: LiveReason::RecentWrite });
        }
    }
    out
}

/// A folder's session files, with their change times (no contents read).
pub fn session_files(host: &dyn Host, claude_home: &Path, folder: &Path) -> Vec<SessionInfo> {
    let dir = project_sessions_dir(claude_home, folder);
    let Ok(entries) = host.read_dir(&dir) else { return Vec::new() };
    entries
        .into_iter()
        .filter(|e| !e.is_dir && e.name.ends_with(".jsonl"))
        .map(|e| {
            let path = dir.join(&e.name);
            SessionInfo {
                id: e.name.trim_end_matches(".jsonl").to_string(),
                modified: host.modified(&path),
                path,
                title: String::new(),
                last_activity: None,
                messages: 0,
            }
        })
        .collect()
}

/// Every `claude` process on this machine except ours, with its folder and start time.
pub fn claude_processes(own_pid: u32) -> Vec<crate::agent::Running> {
    let Ok(out) = crate::platform::command("ps").args(["-axo", "pid=,ppid=,command="]).output() else {
        return Vec::new();
    };
    parse_ps(&String::from_utf8_lossy(&out.stdout))
        .into_iter()
        .filter(|p| p.ppid != own_pid && p.pid != own_pid)
        .filter_map(|p| {
            Some(crate::agent::Running {
                pid: p.pid,
                cwd: process_cwd(p.pid)?,
                session: p.session_arg().map(str::to_string),
                since: process_start(p.pid),
            })
        })
        .collect()
}

/// A process's start time as `HH:MM` (from `ps -o lstart=`: `Sun Oct 11 22:40:00 2026`).
pub fn process_start(pid: u32) -> Option<String> {
    let out = crate::platform::command("ps").args(["-p", &pid.to_string(), "-o", "lstart="]).output().ok()?;
    lstart_time(&String::from_utf8_lossy(&out.stdout))
}

fn lstart_time(lstart: &str) -> Option<String> {
    let time = lstart.split_whitespace().find(|w| w.len() == 8 && w.as_bytes()[2] == b':')?;
    Some(time[..5].to_string())
}

/// The working folder of a process: `proc_pidinfo` on macOS (falling back to `lsof`),
/// `/proc` on Linux. `None` when it cannot be told.
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    if cfg!(target_os = "linux") {
        return std::fs::read_link(format!("/proc/{pid}/cwd")).ok();
    }
    #[cfg(target_os = "macos")]
    if let Some(p) = macos_cwd(pid) {
        return Some(p);
    }
    let out = crate::platform::command("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix('n').map(PathBuf::from))
}

/// `proc_pidinfo(PROC_PIDVNODEPATHINFO)`: the current directory is the path in the first
/// `vnode_info_path` (152 bytes of `vnode_info`, then the path, 1024 bytes).
#[cfg(target_os = "macos")]
fn macos_cwd(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    unsafe extern "C" {
        fn proc_pidinfo(pid: i32, flavor: i32, arg: u64, buffer: *mut std::ffi::c_void, size: i32) -> i32;
    }
    const PROC_PIDVNODEPATHINFO: i32 = 9;
    const SIZE: usize = 2352;
    const PATH_AT: usize = 152;
    let mut buf = vec![0u8; SIZE];
    // SAFETY: the buffer is as large as `struct proc_vnodepathinfo` and the call writes at
    // most `SIZE` bytes into it.
    let n = unsafe { proc_pidinfo(pid as i32, PROC_PIDVNODEPATHINFO, 0, buf.as_mut_ptr().cast(), SIZE as i32) };
    if n as usize != SIZE {
        return None;
    }
    let path = &buf[PATH_AT..PATH_AT + 1024];
    let end = path.iter().position(|b| *b == 0)?;
    (end > 0).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(&path[..end])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::Item;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
        )
        .unwrap()
    }

    #[test]
    fn a_renamed_folder_takes_its_sessions_and_notes_along() {
        let home = std::env::temp_dir().join(format!("kl-move-{}", std::process::id()));
        let (old, new) = (Path::new("/work/keleion"), Path::new("/work/keleuo"));
        let from = project_sessions_dir(&home, old);
        std::fs::create_dir_all(from.join("memory")).unwrap();
        std::fs::write(from.join("a.jsonl"), "a").unwrap();
        std::fs::write(from.join("memory/note.md"), "n").unwrap();
        std::fs::write(from.join("memory/same.md"), "s").unwrap();
        // A sibling whose name starts the same way is left alone.
        let sibling = project_sessions_dir(&home, Path::new("/work/keleion-site"));
        std::fs::create_dir_all(&sibling).unwrap();
        // The new name has a session and a note of its own already.
        let to = project_sessions_dir(&home, new);
        std::fs::create_dir_all(to.join("memory")).unwrap();
        std::fs::write(to.join("b.jsonl"), "b").unwrap();
        std::fs::write(to.join("memory/note.md"), "kept").unwrap();
        std::fs::write(to.join("memory/same.md"), "s").unwrap();
        move_project(&home, old, new).unwrap();
        assert!(to.join("a.jsonl").exists() && to.join("b.jsonl").exists());
        assert_eq!(std::fs::read_to_string(to.join("memory/note.md")).unwrap(), "kept", "the newer copy stays");
        assert!(sibling.exists());
        assert!(from.join("memory/note.md").exists(), "what could not move stays where it was");
        assert!(!from.join("memory/same.md").exists(), "an identical copy is not kept twice");
        // Nothing there: nothing to do.
        move_project(&home, Path::new("/work/none"), Path::new("/work/other")).unwrap();
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_background_subagent_runs_until_its_report_comes_back() {
        use serde_json::json;
        use crate::conversation::{AgentStatus, Item};
        let lines = [
            json!({"type": "assistant", "message": {"id": "m1", "model": "claude", "content": [
                {"type": "tool_use", "id": "toolu_a", "name": "Agent", "input": {"description": "Assess the name", "subagent_type": "general-purpose", "prompt": "..."}}]}}),
            json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_a",
                "content": [{"type": "text", "text": "Async agent launched successfully.\nagentId: a9 (internal ID)"}]}]}}),
            json!({"type": "user", "isMeta": true, "origin": {"kind": "peer", "from": "a9", "handback": true,
                "body": "[Subagent hand-back] Framing. The report follows:\n  Keep the name."},
                "message": {"role": "user", "content": "Another Claude session sent a message:\n<agent-message from=\"a9\">..."}}),
            json!({"type": "user", "message": {"role": "user", "content":
                "<task-notification>\n<tool-use-id>toolu_a</tool-use-id>\n<status>completed</status>\n</task-notification>"}}),
            json!({"type": "user", "message": {"role": "user", "content":
                "<task-notification>\n<tool-use-id>toolu_bash</tool-use-id>\n<status>completed</status>\n</task-notification>"}}),
        ];
        let mut chat = Conversation::new("/tmp");
        chat.replaying = true;
        let mut mapper = Mapper::new("/tmp");
        mapper.history = true;
        chat.apply_all(line_events(&lines[0], &mut mapper));
        chat.apply_all(line_events(&lines[1], &mut mapper));
        assert_eq!(chat.agent("toolu_a").unwrap().status, AgentStatus::Running, "launched, not done");
        chat.apply_all(line_events(&lines[2], &mut mapper));
        assert_eq!(chat.agent("toolu_a").unwrap().status, AgentStatus::Done);
        assert!(matches!(chat.items.last(), Some(Item::AgentMessage { title, text, agent: Some(a), .. })
            if title == "Assess the name" && text == "Keep the name." && a == "toolu_a"));
        chat.apply_all(line_events(&lines[3], &mut mapper));
        chat.apply_all(line_events(&lines[4], &mut mapper));
        assert_eq!(chat.agents.len(), 1, "a shell task's notice is no subagent");
        assert!(!chat.items.iter().any(|i| matches!(i, Item::User(_))), "nothing shows as the person's");
    }

    /// A background agent stopped from outside (its session's process ended) is named only by
    /// its own id; sent another message, it runs again; and when the agent's process ends, a
    /// subagent still running ends with it.
    #[test]
    fn a_background_agent_stopped_resumed_and_ended_with_its_process() {
        use serde_json::json;
        use crate::conversation::AgentStatus;
        let launch = [
            json!({"type": "assistant", "message": {"id": "m1", "model": "claude", "content": [
                {"type": "tool_use", "id": "toolu_a", "name": "Agent", "input": {"description": "Record sessions", "run_in_background": true}}]}}),
            json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_a",
                "content": [{"type": "text", "text": "Async agent launched successfully.\nagentId: a72 (internal ID)"}]}]}}),
        ];
        let stopped = json!({"type": "user", "message": {"role": "user", "content":
            "<task-notification>\n<task-id>a72</task-id>\n<status>stopped</status>\n<summary>didn't finish</summary>\n</task-notification>"}});
        let resume = [
            json!({"type": "assistant", "message": {"id": "m2", "model": "claude", "content": [
                {"type": "tool_use", "id": "toolu_s", "name": "SendMessage", "input": {"to": "a72", "message": "go on"}}]}}),
            json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_s",
                "content": [{"type": "text", "text": "{\"success\":true,\"resumedAgentId\":\"a72\"}"}]}]}}),
        ];
        let mut chat = Conversation::new("/tmp");
        let mut mapper = Mapper::new("/tmp");
        mapper.history = true;
        for l in &launch {
            chat.apply_all(line_events(l, &mut mapper));
        }
        assert_eq!(chat.agent("toolu_a").unwrap().status, AgentStatus::Running);
        chat.apply_all(line_events(&stopped, &mut mapper));
        assert_eq!(chat.agent("toolu_a").unwrap().status, AgentStatus::Failed, "stopped, named by its own id");
        assert_eq!(chat.agents.len(), 1);
        for l in &resume {
            chat.apply_all(line_events(l, &mut mapper));
        }
        assert_eq!(chat.agent("toolu_a").unwrap().status, AgentStatus::Running, "resumed");
        chat.process_ended();
        let agent = chat.agent("toolu_a").unwrap();
        assert_eq!(agent.status, AgentStatus::Failed, "ended with the process");
        assert!(agent.summary.as_deref().unwrap().contains("process ended"));
    }

    #[test]
    fn follows_lines_added_to_a_session_file() {
        use crate::agent::Message;
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex};
        let dir = std::env::temp_dir().join(format!("wb-follow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let text = fixture("session_history.jsonl");
        let lines: Vec<&str> = text.lines().collect();
        std::fs::write(&path, format!("{}\n", lines[0])).unwrap();
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        let sink: crate::agent::Sink = Arc::new(move |m| {
            if let Message::Event(e) = m {
                g.lock().unwrap().push(e)
            }
        });
        let stop = Arc::new(AtomicBool::new(false));
        let (p, s) = (path.clone(), stop.clone());
        let t = std::thread::spawn(move || follow(&crate::host::LocalHost, &p, "/tmp/wb-proj", &sink, &s));
        std::thread::sleep(std::time::Duration::from_millis(200));
        // The rest arrives, the last line without its newline yet.
        let rest = lines[1..].join("\n");
        std::fs::write(&path, format!("{}\n{rest}", lines[0])).unwrap();
        // Until both agents have ended (a busy machine delays the change notice), at most 10 s.
        let ended = |got: &Mutex<Vec<Event>>| got.lock().unwrap().iter().filter(|e| matches!(e, Event::SubagentEnded { .. })).count();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while ended(&got) < 2 && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        t.join().unwrap();
        let mut chat = Conversation::new("/tmp/wb-proj");
        chat.apply_all(got.lock().unwrap().clone());
        // The first line was there before: not replayed. The two Agent calls and their ends came.
        assert_eq!(chat.agents.len(), 2, "{:?}", chat.items.len());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_session_named_on_a_command_line_is_live_even_when_its_folder_is_unknown() {
        use crate::agent::LiveReason;
        // A `claude --resume` started days ago from a path with a space; `lsof` and
        // `proc_pidinfo` both fail for it.
        let procs = parse_ps(
            "  4242  1 /Volumes/Work Disk/bin/claude --resume 2e7649d1-33e0-49c7-a67f-11de7eed02ea --model opus\n\
               4343  1 /usr/local/bin/node /opt/claude-code/cli.js -r aaaa1111\n",
        );
        assert_eq!(procs.len(), 2);
        let s = |id: &str| SessionInfo { id: id.into(), path: PathBuf::new(), title: String::new(), last_activity: None, messages: 1, modified: Some(1) };
        let sessions = [s("2e7649d1-33e0-49c7-a67f-11de7eed02ea"), s("aaaa1111"), s("old")];
        let ages = vec![("old".to_string(), 100_000u64)];
        let found = live_sessions(&Seen {
            processes: &procs,
            cwd_of: &|_| None,
            folder: Path::new("/p"),
            has_session: &|id| sessions.iter().any(|s| s.id == id),
            sessions: &sessions,
            ages: &ages,
            own: &[],
        });
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].pid, found[0].reason), (Some(4242), LiveReason::CommandLine));
        assert_eq!(found[1].id, "aaaa1111");
    }

    #[test]
    fn a_file_written_seconds_ago_may_be_live() {
        use crate::agent::LiveReason;
        let s = |id: &str| SessionInfo { id: id.into(), path: PathBuf::new(), title: String::new(), last_activity: None, messages: 1, modified: None };
        let sessions = [s("fresh"), s("mine"), s("stale")];
        let ages = vec![("fresh".to_string(), 2u64), ("mine".to_string(), 1), ("stale".to_string(), 600)];
        let own = ["mine".to_string()];
        let found = live_sessions(&Seen {
            processes: &[],
            cwd_of: &|_| None,
            folder: Path::new("/p"),
            has_session: &|_| true,
            sessions: &sessions,
            ages: &ages,
            own: &own,
        });
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].id.as_str(), found[0].pid, found[0].reason), ("fresh", None, LiveReason::RecentWrite));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reads_a_process_folder_without_lsof() {
        let here = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(macos_cwd(std::process::id()).map(|p| p.canonicalize().unwrap()), Some(here));
    }

    #[test]
    fn a_compacted_session_loads_its_live_context_and_reads_the_rest_on_request() {
        use crate::conversation::Item;
        let dir = std::env::temp_dir().join(format!("wb-compact-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let user = |t: &str| format!("{{\"type\":\"user\",\"sessionId\":\"s\",\"message\":{{\"role\":\"user\",\"content\":\"{t}\"}}}}");
        let said = |id: &str, t: &str| format!("{{\"type\":\"assistant\",\"message\":{{\"id\":\"{id}\",\"model\":\"claude-haiku-4-5\",\"role\":\"assistant\",\"stop_reason\":\"end_turn\",\"content\":[{{\"type\":\"text\",\"text\":\"{t}\"}}],\"usage\":{{\"input_tokens\":1000,\"output_tokens\":10}}}}}}");
        let boundary = "{\"type\":\"system\",\"subtype\":\"compact_boundary\",\"content\":\"Conversation compacted\",\"compactMetadata\":{\"trigger\":\"auto\",\"preTokens\":150000}}";
        let summary = "{\"type\":\"user\",\"isCompactSummary\":true,\"message\":{\"role\":\"user\",\"content\":\"Summary: we said hello.\"}}";
        let lines = [user("first"), said("m1", "one"), boundary.into(), summary.into(), user("second"), said("m2", "two")];
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let chat = load_streaming(&crate::host::LocalHost, &path, "/p").unwrap();
        let (from, to) = match &chat.items[0] {
            Item::Earlier { from, to, messages } => {
                assert_eq!(*messages, 2);
                (*from, *to)
            }
            other => panic!("{other:?}"),
        };
        assert!(matches!(&chat.items[1], Item::Compacted { summary, tokens: Some(150000), .. } if summary.starts_with("Summary")));
        assert!(matches!(&chat.items[2], Item::User(t) if t == "second"));
        assert_eq!(chat.items.len(), 4);
        // Both answers are counted in the cost, and the context is the last turn's.
        assert!(chat.total_cost_usd > 0.0);
        assert_eq!(chat.last_turn_tokens, Some((1000, 10)));
        let earlier = load_segment(&crate::host::LocalHost, &path, "/p", from, to).unwrap();
        assert!(matches!(&earlier[0], Item::User(t) if t == "first"));
        assert!(matches!(&earlier[1], Item::Assistant { text, .. } if text == "one"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_session_without_a_message_has_nothing_to_resume() {
        let dir = std::env::temp_dir().join(format!("keleuo-hasmsg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let host = crate::host::LocalHost;
        let path = dir.join("s.jsonl");
        assert!(!has_messages(&host, &path), "no file yet");
        std::fs::write(&path, "{\"type\":\"permission-mode\"}\n{\"type\":\"file-history-snapshot\"}\n").unwrap();
        assert!(!has_messages(&host, &path), "only bookkeeping lines");
        std::fs::write(&path, "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n").unwrap();
        assert!(has_messages(&host, &path));
        // From a point on: only what was written after it counts.
        let at = std::fs::metadata(&path).unwrap().len();
        let mut more = std::fs::read_to_string(&path).unwrap();
        more += "{\"type\":\"system\",\"subtype\":\"informational\"}\n";
        std::fs::write(&path, &more).unwrap();
        assert!(!has_messages_from(&host, &path, at), "a notice is not a turn");
        more += "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[]}}\n";
        std::fs::write(&path, &more).unwrap();
        assert!(has_messages_from(&host, &path, at));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reads_the_start_time() {
        assert_eq!(lstart_time("Sun Oct 11 22:40:07 2026\n").as_deref(), Some("22:40"));
        assert_eq!(lstart_time(""), None);
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
    fn a_transcript_whose_agent_call_is_not_in_the_chat_is_left_out() {
        // Its Agent call came before a compaction: attaching it would make a nameless agent
        // that never finishes.
        let mut chat = load_conversation(&fixture("session_history.jsonl"), "/tmp/wb-proj");
        let before = chat.agents.len();
        attach_subagent(&mut chat, "toolu_before_the_compaction", &fixture("subagent_transcript.jsonl"));
        assert_eq!(chat.agents.len(), before);
        assert!(chat.agent("toolu_before_the_compaction").is_none());
    }

    #[test]
    fn messages_delivered_mid_turn_count_like_the_rest() {
        // A message the person sent, a subagent's report and its end, while a turn ran: the
        // interface records them as queued attachments, not user lines.
        let queued = |prompt: &str, origin: Value| {
            serde_json::json!({"type": "attachment", "attachment": {"type": "queued_command", "prompt": prompt, "commandMode": "prompt", "origin": origin}})
        };
        let launch = serde_json::json!({"type": "assistant", "message": {"id": "m1", "role": "assistant", "content": [
            {"type": "tool_use", "id": "toolu_ag", "name": "Agent", "input": {"description": "look around", "subagent_type": "general-purpose", "run_in_background": true}}]}});
        let launched = serde_json::json!({"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_ag", "content": [{"type": "text", "text": "Async agent launched successfully.\nagentId: a123 (internal ID)"}]}]}});
        let lines = [
            launch,
            launched,
            queued("also check the docs", serde_json::json!({"kind": "human"})),
            queued("<agent-message from=\"a123\">\nThe report follows:\n  All good.\n</agent-message>", serde_json::json!({"kind": "human"})),
            queued("<task-notification>\n<task-id>a123</task-id>\n<tool-use-id>toolu_ag</tool-use-id>\n<status>completed</status>\n</task-notification>", serde_json::json!({"kind": "human"})),
        ];
        let text: String = lines.iter().map(|l| l.to_string() + "\n").collect();
        let chat = load_conversation(&text, "/p");
        assert!(chat.items.iter().any(|i| matches!(i, Item::User(t) if t == "also check the docs")), "{:?}", chat.items);
        assert!(chat.items.iter().any(|i| matches!(i, Item::AgentMessage { text, .. } if text.contains("All good"))));
        assert!(chat.agents.iter().all(|a| !a.running()), "the subagent finished");
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
        let folder = Path::new("/p");
        let ages: Vec<(String, u64)> = vec![("s1".into(), 3000), ("s2".into(), 1000), ("s3".into(), 2000)];
        let seen = |cwd: &'static dyn Fn(u32) -> Option<PathBuf>, procs: &[ClaudeProcess]| {
            live_sessions(&Seen {
                processes: procs,
                cwd_of: cwd,
                folder,
                has_session: &|id| sessions.iter().any(|s| s.id == id),
                sessions: &sessions,
                ages: &ages,
                own: &[],
            })
        };
        let found = seen(&|_| Some(PathBuf::from("/p")), &procs[..2]);
        let pairs: Vec<(Option<u32>, &str)> = found.iter().map(|l| (l.pid, l.id.as_str())).collect();
        assert_eq!(pairs, [(Some(101), "s2"), (Some(102), "s3")]);
    }
}
