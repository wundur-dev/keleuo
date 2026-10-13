//! Claude Code, over its `stream-json` protocol. Everything Claude specific lives in this
//! module: the command line ([`process`]), the wire format ([`stream`]) and its mapping onto
//! neutral events ([`map`]), session files under `~/.claude/projects` ([`history`]), tool card
//! summaries ([`summary`]) and where skills, agents and commands are kept ([`skills`]).

pub mod history;
pub mod hooks;
pub mod issues;
pub mod live;
pub mod map;
pub mod messages;
pub mod pricing;
pub mod process;
pub mod screen;
pub mod skills;
pub mod stream;
pub mod summary;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::agent::{Agent, Backend, Capabilities, Follow, Hook, LiveSession, Message, Running, SessionInfo, Sink, StartOptions};
use crate::conversation::Conversation;
use crate::host::{Host, ProgramSpec};
use map::Mapper;
use process::{Line, Options, Process};

/// The id saved with sessions.
pub const ID: &str = "claude-code";

/// Whether Claude Code's own record (`~/.claude.json`) says the folder's trust prompt was
/// accepted: `projects.<folder>.hasTrustDialogAccepted`.
pub fn trusted_in(claude_json: &str, folder: &str) -> bool {
    serde_json::from_str::<Value>(claude_json)
        .ok()
        .and_then(|v| v.get("projects")?.get(folder)?.get("hasTrustDialogAccepted")?.as_bool())
        .unwrap_or(false)
}

/// Claude Code as a [`Backend`].
#[derive(Default)]
pub struct ClaudeBackend;

impl ClaudeBackend {
    /// The folder's session files with their change times only (no contents read).
    fn earlier_sessions_quick(&self, host: &dyn Host, project: &Path) -> Vec<SessionInfo> {
        let Some(home) = skills::claude_home(host) else { return Vec::new() };
        history::session_files(host, &home, project)
    }

    fn program_name(&self) -> String {
        "claude".into()
    }
}

impl Backend for ClaudeBackend {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Claude Code"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            skills: true,
            earlier_sessions: true,
            fork: true,
            subagents: true,
            permission_prompts: true,
            accept_edits_mode: Some("acceptEdits".into()),
            // Claude Code's aliases, each the latest of its family; any other name is typed in.
            models: ["fable", "opus", "sonnet", "haiku"].map(String::from).into(),
            terminal_view: true,
        }
    }

    fn new_agent(&self) -> Box<dyn Agent> {
        Box::new(ClaudeCode::new())
    }

    fn skills(&self, host: &dyn Host, project: &Path) -> crate::skills::Catalog {
        skills::discover(host, project, skills::claude_home(host).as_deref()).into_neutral()
    }

    fn create_skill(&self, host: &dyn Host, project: &Path, name: &str) -> std::io::Result<PathBuf> {
        skills::create_project_skill(host, project, name)
    }

    fn earlier_sessions(&self, host: &dyn Host, project: &Path, max: usize) -> Vec<SessionInfo> {
        match skills::claude_home(host) {
            Some(home) => history::list_sessions(host, &home, project, max),
            None => Vec::new(),
        }
    }

    fn session_count(&self, host: &dyn Host, project: &Path) -> (usize, Option<u64>) {
        match skills::claude_home(host) {
            Some(home) => history::session_count(host, &home, project),
            None => (0, None),
        }
    }

    fn load_session(&self, host: &dyn Host, project: &Path, id: &str) -> std::io::Result<Conversation> {
        let home = skills::claude_home(host).ok_or_else(|| std::io::Error::other("no home folder"))?;
        let path = history::project_sessions_dir(&home, project).join(format!("{id}.jsonl"));
        history::load_session(host, &path, &project.to_string_lossy())
    }

    fn session_has_messages(&self, host: &dyn Host, project: &Path, id: &str) -> bool {
        let Some(home) = skills::claude_home(host) else { return false };
        let path = history::project_sessions_dir(&home, project).join(format!("{id}.jsonl"));
        history::has_messages(host, &path)
    }

    fn load_segment(&self, host: &dyn Host, project: &Path, id: &str, from: u64, to: u64) -> std::io::Result<Vec<crate::conversation::Item>> {
        let home = skills::claude_home(host).ok_or_else(|| std::io::Error::other("no home folder"))?;
        let path = history::project_sessions_dir(&home, project).join(format!("{id}.jsonl"));
        history::load_segment(host, &path, &project.to_string_lossy(), from, to)
    }

    fn terminal_program(&self, options: &StartOptions, id: &str, new: bool) -> Option<ProgramSpec> {
        let mut args: Vec<String> = Vec::new();
        args.extend([if new { "--session-id" } else { "--resume" }.to_string(), id.to_string()]);
        if options.fork && !new {
            args.push("--fork-session".into());
        }
        if let Some(settings) = &options.settings {
            args.extend(["--settings".into(), settings.to_string_lossy().to_string()]);
        }
        if let Some(model) = &options.model {
            args.extend(["--model".into(), model.clone()]);
        }
        if let Some(mode) = &options.permission_mode {
            args.extend(["--permission-mode".into(), mode.clone()]);
        }
        Some(ProgramSpec {
            program: self.program_name(),
            args,
            env: Vec::new(),
            unset: process::inherited_session_vars(),
            cwd: options.cwd.clone(),
        })
    }

    fn follow_session(&self, host: Arc<dyn Host>, project: &Path, id: &str, sink: Sink) -> Option<Follow> {
        let home = skills::claude_home(host.as_ref())?;
        let path = history::project_sessions_dir(&home, project).join(format!("{id}.jsonl"));
        let (follow, stop) = Follow::new();
        let cwd = project.to_string_lossy().to_string();
        std::thread::spawn(move || history::follow(host.as_ref(), &path, &cwd, &sink, &stop));
        Some(follow)
    }

    fn hook_settings(&self, command: &dyn Fn(&str) -> String) -> Option<String> {
        Some(hooks::settings(command))
    }

    fn parse_hook(&self, event: &str, payload: &Value, cwd: &str) -> Option<Hook> {
        hooks::parse(event, payload, cwd)
    }

    fn hook_reply(&self, allow: bool) -> String {
        hooks::reply(allow)
    }

    fn terminal_attach(&self, path: &Path, bracketed: bool) -> Option<Vec<u8>> {
        // A pasted image path becomes `[Image #N]`, the image itself attached.
        let p = path.to_string_lossy();
        Some(if bracketed { format!("\x1b[200~{p}\x1b[201~") } else { p.to_string() }.into_bytes())
    }

    fn terminal_paste_image(&self) -> Option<Vec<u8>> {
        // Claude Code reads an image from the clipboard on Ctrl+V.
        Some(vec![0x16])
    }

    fn model_command(&self, model: &str) -> Option<String> {
        Some(format!("/model {model}"))
    }

    fn mode_cycle(&self) -> Option<crate::agent::ModeCycle> {
        let modes = [("default", ""), ("acceptEdits", "accept edits on"), ("plan", "plan mode on"), ("auto", "auto mode on")];
        // Shift+Tab cycles the modes.
        Some((modes.iter().map(|(m, l)| (m.to_string(), l.to_string())).collect(), b"\x1b[Z".to_vec()))
    }

    fn hook_mode(&self, payload: &Value) -> Option<String> {
        payload.get("permission_mode").and_then(Value::as_str).map(str::to_string)
    }

    fn billed_per_use(&self, host: &dyn Host, project: &Path) -> Option<bool> {
        let env = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
        let settings = [
            skills::claude_home(host).map(|h| h.join("settings.json")),
            Some(project.join(".claude/settings.json")),
            Some(project.join(".claude/settings.local.json")),
        ];
        let texts: Vec<String> = settings.into_iter().flatten().filter_map(|p| host.read_to_string(&p).ok()).collect();
        Some(pricing::billed_per_use(!host.is_remote(), &env, &texts))
    }

    fn folder_trusted(&self, host: &dyn Host, project: &Path) -> Option<bool> {
        // Only this folder's flag is looked at; nothing else in the file is used or shown.
        let text = host.read_to_string(&host.home_dir()?.join(".claude.json")).ok()?;
        Some(trusted_in(&text, &project.to_string_lossy()))
    }

    fn context_window(&self, model: &str) -> Option<u64> {
        Some(pricing::context_window(model))
    }

    fn running(&self, host: &dyn Host, own_pid: u32) -> Vec<Running> {
        if host.is_remote() {
            return Vec::new();
        }
        history::claude_processes(own_pid)
    }

    fn live_sessions(&self, host: &dyn Host, project: &Path, sessions: &[SessionInfo], own_pid: u32, own: &[String]) -> Vec<LiveSession> {
        let Some(home) = skills::claude_home(host) else { return Vec::new() };
        let dir = history::project_sessions_dir(&home, project);
        let processes = if host.is_remote() { Vec::new() } else { history::other_claude_processes(own_pid) };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let recent: Vec<(String, u64)> = sessions
            .iter()
            .filter_map(|s| Some((s.id.clone(), now.saturating_sub(host.modified(&s.path)?))))
            .collect();
        history::live_sessions(&history::Seen {
            processes: &processes,
            cwd_of: &history::process_cwd,
            folder: project,
            has_session: &|id| host.exists(&dir.join(format!("{id}.jsonl"))),
            sessions,
            ages: &recent,
            own,
        })
    }

    fn reply_preview(&self, lines: &[String], columns: usize) -> Option<String> {
        screen::reply_in_progress(lines, columns)
    }

    fn message_unsent(&self, lines: &[String], text: &str) -> bool {
        screen::message_unsent(lines, text)
    }

    fn apply_fix(&self, host: &dyn Host, project: &Path, kind: &str, fix: &str) -> std::io::Result<String> {
        if host.is_remote() {
            return Err(std::io::Error::other("fixes are applied on this machine only for now"));
        }
        issues::apply(kind, fix, project, dirs::home_dir().as_deref())
    }

    fn folder_renamed(&self, host: &dyn Host, old: &Path, new: &Path) -> std::io::Result<()> {
        match skills::claude_home(host) {
            Some(home) if !host.is_remote() => history::move_project(&home, old, new),
            _ => Ok(()),
        }
    }

    fn session_live(&self, host: &dyn Host, project: &Path, id: &str, own_pid: u32) -> Option<LiveSession> {
        let home = skills::claude_home(host)?;
        let path = history::project_sessions_dir(&home, project).join(format!("{id}.jsonl"));
        let info = SessionInfo { id: id.into(), path, title: String::new(), last_activity: None, messages: 0, modified: None };
        // The folder's newest sessions too, so a process found by its folder is matched right.
        let mut sessions = self.earlier_sessions_quick(host, project);
        if !sessions.iter().any(|s| s.id == id) {
            sessions.push(info);
        }
        self.live_sessions(host, project, &sessions, own_pid, &[]).into_iter().find(|l| l.id == id)
    }
}

/// One Claude Code process: its lines are parsed and mapped onto neutral events on the
/// reader thread, so the window only ever sees [`crate::agent::Event`]s.
#[derive(Default)]
pub struct ClaudeCode {
    process: Option<Process>,
    /// Another `claude` program (tests use a fake one).
    program: Option<PathBuf>,
}

impl ClaudeCode {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_program(program: PathBuf) -> Self {
        Self { process: None, program: Some(program) }
    }

    fn process(&mut self) -> std::io::Result<&mut Process> {
        self.process.as_mut().ok_or_else(|| std::io::Error::other("Claude Code is not running"))
    }
}

impl Agent for ClaudeCode {
    fn running(&self) -> bool {
        self.process.is_some()
    }

    fn start(&mut self, host: &dyn Host, options: &StartOptions, sink: Sink) -> anyhow::Result<()> {
        let spec = host.program_command(
            &Options {
                program: self.program.clone(),
                cwd: options.cwd.clone(),
                model: options.model.clone(),
                permission_mode: options.permission_mode.clone(),
                resume: options.resume.clone(),
                fork: options.fork,
            }
            .program_spec(),
        );
        let mapper = Mutex::new(Mapper::new(options.cwd.to_string_lossy()));
        let sink: Sink = sink;
        let forward = Arc::new(move |line: Line| match line {
            Line::Stdout(text) => {
                let Some(raw) = stream::parse_line(&text) else { return };
                let events = mapper.lock().map(|mut m| m.map(raw)).unwrap_or_default();
                for e in events {
                    sink(Message::Event(e));
                }
            }
            Line::Stderr(text) => sink(Message::Log(text)),
            Line::Closed => sink(Message::Closed),
        });
        self.process = Some(Process::spawn(&spec, move |l| forward(l))?);
        Ok(())
    }

    fn send(&mut self, text: &str) -> std::io::Result<()> {
        self.process()?.send_user(text)
    }

    fn answer_permission(&mut self, request_id: &str, allow: bool, input: &Value) -> std::io::Result<()> {
        self.process()?.send_line(&stream::permission_response(request_id, allow, input))
    }

    fn interrupt(&mut self) -> std::io::Result<()> {
        self.process()?.interrupt()
    }

    fn stop(&mut self) {
        if let Some(mut p) = self.process.take() {
            p.kill();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {

    #[test]
    fn reads_only_the_folder_trust_flag() {
        let json = r#"{"projects": {"/a": {"hasTrustDialogAccepted": true}, "/b": {"allowedTools": []}}}"#;
        assert!(super::trusted_in(json, "/a"));
        assert!(!super::trusted_in(json, "/b"));
        assert!(!super::trusted_in(json, "/c"));
        assert!(!super::trusted_in("not json", "/a"));
    }
    use super::*;
    use crate::agent::Event;
    use crate::host::LocalHost;

    /// A fake `claude` that answers every stdin line with an assistant message, through the
    /// trait object: the window would see neutral events only.
    #[test]
    fn claude_code_through_the_trait() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("wb-agent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("claude");
        std::fs::write(
            &script,
            "#!/bin/sh\nwhile read line; do echo '{\"type\":\"assistant\",\"message\":{\"id\":\"m\",\"model\":\"fake\",\"content\":[{\"type\":\"text\",\"text\":\"hello\"}]}}'; echo oops >&2; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let got = Arc::new(Mutex::new(Vec::new()));
        let sink_got = got.clone();
        let mut agent: Box<dyn Agent> = Box::new(ClaudeCode::with_program(script));
        assert!(!agent.running());
        agent
            .start(
                &LocalHost,
                &StartOptions { cwd: dir.clone(), ..Default::default() },
                Arc::new(move |m| sink_got.lock().unwrap().push(m)),
            )
            .unwrap();
        assert!(agent.running());
        agent.send("hi").unwrap();
        let start = std::time::Instant::now();
        while got.lock().unwrap().len() < 3 && start.elapsed().as_secs() < 5 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        agent.stop();
        assert!(!agent.running());
        let got = got.lock().unwrap();
        assert!(got.iter().any(|m| matches!(m, Message::Event(Event::Text { text, .. }) if text == "hello")), "{got:?}");
        assert!(got.iter().any(|m| matches!(m, Message::Log(t) if t == "oops")), "{got:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn backend_says_what_it_has() {
        let b = ClaudeBackend;
        assert_eq!(b.id(), ID);
        let caps = b.capabilities();
        assert!(caps.skills && caps.earlier_sessions && caps.subagents && caps.permission_prompts);
    }
}
