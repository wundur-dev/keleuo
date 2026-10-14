//! Cursor Agent (the `cursor-agent` command line), headless. Everything Cursor specific lives
//! in this module: the command line (here) and its output mapped onto neutral events ([`map`]).
//!
//! Unlike Claude Code it takes no input stream: each message is a run of its own,
//! `cursor-agent -p` with the message on stdin, continuing one chat (`--resume <chat id>`,
//! the id made with `create-chat` before the first message). Stopping a turn ends that run.
//!
//! It never asks before acting when run like this. What it may do is its permission mode:
//! the person's Cursor allowlist (`~/.cursor/cli-config.json`; anything else is refused),
//! everything (`--force`), or read-only planning or questions (`--mode plan|ask`).

pub mod map;
pub mod store;

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::agent::{Agent, Backend, Capabilities, Event, Message, ModelOption, Sink, StartOptions};
use crate::conversation::Conversation;
use crate::host::{Host, ProgramSpec};
use map::Mapper;

/// The id saved with sessions.
pub const ID: &str = "cursor";

/// The permission modes: Keleuo's name, and what the window calls it.
const MODES: [(&str, &str); 4] =
    [("default", "Allowlist only"), ("force", "Run everything"), ("plan", "Plan (read-only)"), ("ask", "Ask (read-only)")];

/// Cursor Agent as a [`Backend`].
#[derive(Default)]
pub struct CursorBackend {
    /// The account's models, listed once per run of the window.
    models: Mutex<Option<Vec<ModelOption>>>,
}

impl Backend for CursorBackend {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Cursor"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // A few of `cursor-agent models`; any other name is typed in.
            models: ["auto", "composer-2.5", "claude-sonnet-5-thinking-high", "claude-opus-5-thinking-high", "gpt-5.3-codex"]
                .map(String::from)
                .into(),
            ..Default::default()
        }
    }

    fn new_agent(&self) -> Box<dyn Agent> {
        Box::new(CursorAgent::default())
    }

    fn permission_modes(&self) -> Vec<(&'static str, &'static str)> {
        MODES.to_vec()
    }

    /// `cursor-agent models`: every model and variant (effort, fast) the account may use.
    fn list_models(&self, host: &dyn Host) -> Vec<ModelOption> {
        if let Some(known) = self.models.lock().unwrap().clone() {
            return known;
        }
        let spec = ProgramSpec { program: "cursor-agent".into(), args: vec!["models".into()], ..Default::default() };
        let Ok(out) = host.program_command(&spec).to_command().stdin(Stdio::null()).output() else { return Vec::new() };
        let models = parse_models(&String::from_utf8_lossy(&out.stdout));
        if !models.is_empty() {
            *self.models.lock().unwrap() = Some(models.clone());
        }
        models
    }

    /// The chat as Cursor recorded it ([`store`]). When Keleuo cannot read Cursor's record (a
    /// layout it does not know yet), the session opens with no earlier messages and a note;
    /// its next message still continues the chat.
    fn load_session(&self, host: &dyn Host, project: &Path, id: &str) -> std::io::Result<Conversation> {
        match store::load(host, project, id) {
            Ok(chat) => Ok(chat),
            Err(e) => {
                let mut chat = Conversation::new(project.to_string_lossy());
                chat.session_id = Some(id.to_string());
                chat.push_notice(format!(
                    "Earlier messages of this Cursor chat are not shown here ({e}); Cursor still has them, and the next message continues the chat."
                ));
                Ok(chat)
            }
        }
    }
}

/// The lines of `cursor-agent models` (`<id> - <name>`, after a heading), in its order.
fn parse_models(text: &str) -> Vec<ModelOption> {
    text.lines()
        .filter_map(|line| {
            let (id, label) = line.trim().split_once(" - ")?;
            if id.is_empty() || id.contains(' ') {
                return None;
            }
            let label = label.split(" (current").next().unwrap_or(label).trim();
            Some(ModelOption { id: id.into(), label: label.into(), family: family(id).into() })
        })
        .collect()
}

/// The family a model id belongs to, for the menu ("" for Auto, which stands alone).
fn family(id: &str) -> &'static str {
    let id = id.strip_prefix("cursor-").unwrap_or(id);
    match id.split(['-', '.']).next().unwrap_or("") {
        "auto" => "",
        "claude" => "Claude",
        "gpt" | "o3" | "o4" | "codex" => "GPT",
        "gemini" => "Gemini",
        "grok" => "Grok",
        "composer" => "Composer",
        "kimi" => "Kimi",
        "glm" => "GLM",
        "deepseek" => "DeepSeek",
        _ => "Other",
    }
}

/// The flags for a permission mode. A mode of another agent's (a default from the settings)
/// counts as the nearest one here.
fn mode_args(mode: Option<&str>) -> Vec<String> {
    match mode {
        Some("force" | "bypassPermissions") => vec!["--force".into()],
        Some(m @ ("plan" | "ask")) => vec!["--mode".into(), m.into()],
        _ => Vec::new(),
    }
}

/// The run for one message.
fn run_args(options: &StartOptions, chat: &str) -> Vec<String> {
    let mut args: Vec<String> = ["-p", "--output-format", "stream-json", "--stream-partial-output", "--trust"].map(String::from).into();
    if let Some(model) = &options.model {
        args.extend(["--model".into(), model.clone()]);
    }
    args.extend(mode_args(options.permission_mode.as_deref()));
    args.extend(["--resume".into(), chat.into()]);
    args
}

/// A conversation with Cursor Agent: one `cursor-agent` run per message.
#[derive(Default)]
pub struct CursorAgent {
    host: Option<Arc<dyn Host>>,
    options: StartOptions,
    sink: Option<Sink>,
    /// The chat every run continues; made before the first message when there is none yet.
    chat: Arc<Mutex<Option<String>>>,
    /// The latest run (each has a slot of its own: one may still be ending as the next starts).
    current: Arc<Mutex<Option<Child>>>,
    /// The person stopped the run under way.
    stopped: Arc<AtomicBool>,
}

impl CursorAgent {
    fn spec(&self, args: Vec<String>) -> ProgramSpec {
        ProgramSpec { program: "cursor-agent".into(), args, cwd: self.options.cwd.clone(), ..Default::default() }
    }
}

impl Agent for CursorAgent {
    fn running(&self) -> bool {
        self.sink.is_some()
    }

    fn start(&mut self, host: Arc<dyn Host>, options: &StartOptions, sink: Sink) -> anyhow::Result<()> {
        *self.chat.lock().unwrap() = options.resume.clone();
        self.host = Some(host);
        self.options = options.clone();
        sink(Message::Event(Event::Started {
            session_id: options.resume.clone(),
            model: options.model.clone(),
            permission_mode: options.permission_mode.clone(),
            cwd: Some(options.cwd.to_string_lossy().to_string()),
        }));
        self.sink = Some(sink);
        Ok(())
    }

    fn send(&mut self, text: &str) -> std::io::Result<()> {
        let (Some(host), Some(sink)) = (self.host.clone(), self.sink.clone()) else {
            return Err(std::io::Error::other("Cursor is not started"));
        };
        let create = host.program_command(&self.spec(vec!["create-chat".into()]));
        let options = self.options.clone();
        self.current = Arc::new(Mutex::new(None));
        self.stopped = Arc::new(AtomicBool::new(false));
        let (chat, current, stopped) = (self.chat.clone(), self.current.clone(), self.stopped.clone());
        let base = self.spec(Vec::new());
        let text = text.to_string();
        // Off the window's thread: making the chat and starting the run take a moment.
        std::thread::spawn(move || {
            let end = |error: String| {
                sink(Message::Event(Event::TurnEnded {
                    error: Some(error),
                    cost_total_usd: None,
                    input_tokens: 0,
                    output_tokens: 0,
                    duration_ms: None,
                    session_id: None,
                }))
            };
            let known = chat.lock().unwrap().clone();
            let id = match known {
                Some(id) => id,
                None => match create.to_command().output() {
                    Ok(out) if out.status.success() => {
                        let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
                        *chat.lock().unwrap() = Some(id.clone());
                        id
                    }
                    Ok(out) => return end(format!("cursor-agent create-chat failed: {}", String::from_utf8_lossy(&out.stderr).trim())),
                    Err(e) => return end(format!("could not start cursor-agent: {e}")),
                },
            };
            let spec = host.program_command(&ProgramSpec { args: run_args(&options, &id), ..base });
            // A group of its own: the run starts a worker that writes the reply, and stopping
            // the run must end that too.
            let mut cmd = spec.to_command();
            crate::platform::own_group(&mut cmd);
            let child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
            let mut child = match child {
                Ok(c) => c,
                Err(e) => return end(format!("could not start cursor-agent: {e}")),
            };
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let (stdout, mut stderr) = (child.stdout.take(), child.stderr.take());
            *current.lock().unwrap() = Some(child);
            // Stopped while it was starting.
            if stopped.load(Ordering::Relaxed)
                && let Some(c) = current.lock().unwrap().as_mut()
            {
                crate::platform::kill_group(c);
            }
            let errors = std::thread::spawn(move || {
                let mut text = String::new();
                if let Some(e) = stderr.as_mut() {
                    let _ = e.read_to_string(&mut text);
                }
                text
            });
            let mut mapper = Mapper::new(options.cwd.to_string_lossy(), options.permission_mode.clone());
            for line in stdout.into_iter().flat_map(|o| BufReader::new(o).lines()) {
                let Ok(line) = line else { break };
                // The run's start names the chat, which the session keeps to continue it later.
                for e in mapper.map_line(&line) {
                    sink(Message::Event(e));
                }
            }
            // Its output is done; wait for it to exit without holding the slot, so a stop can
            // still reach it.
            loop {
                let exited = match current.lock().unwrap().as_mut() {
                    Some(c) => !matches!(c.try_wait(), Ok(None)),
                    None => true,
                };
                if exited {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            // The run leaves its worker behind (a minute or more); end it with the run.
            if let Some(mut c) = current.lock().unwrap().take() {
                crate::platform::kill_group(&mut c);
            }
            let errors = errors.join().unwrap_or_default();
            if stopped.load(Ordering::Relaxed) {
                sink(Message::Event(Event::Interrupted));
            } else if !mapper.ended {
                let last = errors.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("it ended without an answer");
                end(format!("Cursor: {}", last.trim()));
            }
            for line in errors.lines().filter(|l| !l.trim().is_empty()) {
                sink(Message::Log(line.to_string()));
            }
        });
        Ok(())
    }

    fn answer_permission(&mut self, _request_id: &str, _allow: bool, _input: &Value) -> std::io::Result<()> {
        // Cursor never asks when run like this.
        Ok(())
    }

    fn interrupt(&mut self) -> std::io::Result<()> {
        self.stopped.store(true, Ordering::Relaxed);
        if let Some(c) = self.current.lock().unwrap().as_mut() {
            crate::platform::kill_group(c);
        }
        Ok(())
    }

    fn stop(&mut self) {
        let _ = self.interrupt();
        self.sink = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mode_of_another_agent_counts_as_the_nearest() {
        assert_eq!(mode_args(Some("force")), ["--force"]);
        assert_eq!(mode_args(Some("bypassPermissions")), ["--force"]);
        assert_eq!(mode_args(Some("plan")), ["--mode", "plan"]);
        assert_eq!(mode_args(Some("ask")), ["--mode", "ask"]);
        // Claude's Auto (the settings' default) and the rest: the person's allowlist.
        assert!(mode_args(Some("auto")).is_empty());
        assert!(mode_args(None).is_empty());
    }

    #[test]
    fn the_model_list_reads_as_families() {
        let text = "Available models\n\nauto - Auto (current, default)\ngpt-5.3-codex-low - Codex 5.3 Low\n\
            claude-opus-5-thinking-high - Claude Opus 5 1M Thinking\ncursor-grok-4.5-high - Grok 4.5\n\
            grok-4.7-low-fast - Grok 4.7  Low Fast\ncomposer-2.5 - Composer 2.5\nmuse-spark-1.3 - Muse Spark\n\n\
            Tip: use --model <id> (or /model <id> in interactive mode) to switch.\n";
        let models = parse_models(text);
        let got: Vec<(&str, &str, &str)> = models.iter().map(|m| (m.id.as_str(), m.label.as_str(), m.family.as_str())).collect();
        assert_eq!(
            got,
            [
                ("auto", "Auto", ""),
                ("gpt-5.3-codex-low", "Codex 5.3 Low", "GPT"),
                ("claude-opus-5-thinking-high", "Claude Opus 5 1M Thinking", "Claude"),
                ("cursor-grok-4.5-high", "Grok 4.5", "Grok"),
                ("grok-4.7-low-fast", "Grok 4.7  Low Fast", "Grok"),
                ("composer-2.5", "Composer 2.5", "Composer"),
                ("muse-spark-1.3", "Muse Spark", "Other"),
            ]
        );
    }

    #[test]
    fn every_run_continues_the_chat() {
        let options = StartOptions { model: Some("auto".into()), permission_mode: Some("plan".into()), ..Default::default() };
        let args = run_args(&options, "c1");
        assert_eq!(&args[..2], ["-p", "--output-format"]);
        assert!(args.windows(2).any(|w| w == ["--model", "auto"]));
        assert!(args.windows(2).any(|w| w == ["--mode", "plan"]));
        assert!(args.ends_with(&["--resume".into(), "c1".into()]));
    }

    /// Against the real `cursor-agent` (signed in; spends a little): two messages in one chat,
    /// the second remembering the first, then a turn stopped part way.
    /// `cargo test -p keleuo-core cursor_for_real -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn cursor_for_real() {
        use std::sync::mpsc::channel;
        use std::time::Duration;
        let dir = std::env::temp_dir().join(format!("keleuo-cursor-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = channel::<Message>();
        let tx = Mutex::new(tx);
        let mut agent = CursorAgent::default();
        let options = StartOptions { cwd: dir.clone(), model: Some("auto".into()), ..Default::default() };
        agent.start(Arc::new(crate::host::LocalHost), &options, Arc::new(move |m| drop(tx.lock().unwrap().send(m)))).unwrap();
        let turn = |agent: &mut CursorAgent, text: &str| {
            agent.send(text).unwrap();
            let mut got = Vec::new();
            loop {
                match rx.recv_timeout(Duration::from_secs(120)).expect("the turn ends") {
                    Message::Event(e @ (Event::TurnEnded { .. } | Event::Interrupted)) => {
                        got.push(e);
                        return got;
                    }
                    Message::Event(e) => got.push(e),
                    Message::Log(l) => eprintln!("log: {l}"),
                    Message::Closed => panic!("closed"),
                }
            }
        };
        let reply = |events: &[Event]| -> String {
            events.iter().filter_map(|e| if let Event::Text { text, .. } = e { Some(text.clone()) } else { None }).collect()
        };
        let first = turn(&mut agent, "Remember the word heron. Reply with just: ok");
        eprintln!("first: {:?}", reply(&first));
        let id = agent.chat.lock().unwrap().clone().expect("a chat was made");
        assert!(first.iter().any(|e| matches!(e, Event::Started { session_id: Some(s), .. } if *s == id)));
        let second = turn(&mut agent, "What word did I ask you to remember? Reply with just the word.");
        eprintln!("second: {:?}", reply(&second));
        assert!(reply(&second).to_lowercase().contains("heron"));
        assert!(matches!(second.last(), Some(Event::TurnEnded { error: None, .. })));
        // Stopped part way: the run ends, as interrupted.
        agent.send("Write a 2000-word story about a heron, in plain prose.").unwrap();
        std::thread::sleep(Duration::from_secs(4));
        agent.interrupt().unwrap();
        let mut last = None;
        while let Ok(m) = rx.recv_timeout(Duration::from_secs(30)) {
            if let Message::Event(e @ (Event::TurnEnded { .. } | Event::Interrupted)) = m {
                last = Some(e);
                break;
            }
        }
        assert_eq!(last, Some(Event::Interrupted));
        agent.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
