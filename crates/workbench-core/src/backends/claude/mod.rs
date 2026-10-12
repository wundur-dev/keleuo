//! Claude Code, over its `stream-json` protocol. Everything Claude specific lives in this
//! module: the command line ([`process`]), the wire format ([`stream`]) and its mapping onto
//! neutral events ([`map`]), session files under `~/.claude/projects` ([`history`]), tool card
//! summaries ([`summary`]) and where skills, agents and commands are kept ([`skills`]).

pub mod history;
pub mod map;
pub mod process;
pub mod skills;
pub mod stream;
pub mod summary;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::agent::{Agent, Backend, Capabilities, Message, Running, SessionInfo, Sink, StartOptions};
use crate::conversation::Conversation;
use crate::host::Host;
use map::Mapper;
use process::{Line, Options, Process};

/// The id saved with sessions.
pub const ID: &str = "claude-code";

/// Claude Code as a [`Backend`].
#[derive(Default)]
pub struct ClaudeBackend;

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
            models: ["haiku", "sonnet", "opus"].map(String::from).into(),
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

    fn running(&self, host: &dyn Host, own_pid: u32) -> Vec<Running> {
        if host.is_remote() {
            return Vec::new();
        }
        history::claude_processes(own_pid)
    }

    fn running_elsewhere(&self, host: &dyn Host, project: &Path, sessions: &[SessionInfo], own_pid: u32) -> Vec<(u32, String)> {
        if host.is_remote() {
            return Vec::new();
        }
        history::match_processes(&history::claude_processes_in(project, own_pid), sessions)
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
