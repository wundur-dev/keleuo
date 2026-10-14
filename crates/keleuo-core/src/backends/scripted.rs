//! A fake agent for tests: it plays a fixed turn of neutral events (streamed text, a tool call
//! with a diff, a permission prompt it waits on, a subagent, usage and cost) for every message.
//! It proves the window works with a backend that is not Claude Code, and drives the same UI
//! paths without spending anything. It never touches files.

use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::agent::{Agent, Backend, Capabilities, Event, Message, Sink, StartOptions};
use crate::diff::{self, FileDiff};
use crate::host::Host;

pub const ID: &str = "scripted";

/// The fake backend. `step`: the pause between events (zero in unit tests, slower when a
/// person is watching the window).
pub struct ScriptedBackend {
    pub step: Duration,
}

impl Default for ScriptedBackend {
    fn default() -> Self {
        Self { step: Duration::ZERO }
    }
}

impl Backend for ScriptedBackend {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Scripted agent (test)"
    }

    /// Subagents and permission prompts, nothing else: the window must hide skills, earlier
    /// sessions, forking, the accept-edits toggle and the model choice.
    fn capabilities(&self) -> Capabilities {
        Capabilities { subagents: true, permission_prompts: true, ..Default::default() }
    }

    fn new_agent(&self) -> Box<dyn Agent> {
        Box::new(ScriptedAgent { step: self.step, ..Default::default() })
    }

    fn session_count(&self, _host: &dyn Host, _project: &Path) -> (usize, Option<u64>) {
        (0, None)
    }
}

/// The open permission prompt's id and where its answer goes.
type Pending = Option<(String, Sender<bool>)>;

#[derive(Default)]
pub struct ScriptedAgent {
    step: Duration,
    sink: Option<Sink>,
    cwd: String,
    turns: u32,
    cost: f64,
    /// The pending permission prompt's answer channel.
    answer: Arc<Mutex<Pending>>,
}

impl Agent for ScriptedAgent {
    fn running(&self) -> bool {
        self.sink.is_some()
    }

    fn start(&mut self, _host: Arc<dyn Host>, options: &StartOptions, sink: Sink) -> anyhow::Result<()> {
        self.cwd = options.cwd.to_string_lossy().to_string();
        sink(Message::Event(Event::Started {
            session_id: Some(format!("scripted-{}", std::process::id())),
            model: Some(options.model.clone().unwrap_or_else(|| "scripted".into())),
            permission_mode: options.permission_mode.clone(),
            cwd: Some(self.cwd.clone()),
        }));
        self.sink = Some(sink);
        Ok(())
    }

    fn send(&mut self, text: &str) -> std::io::Result<()> {
        let sink = self.sink.clone().ok_or_else(|| std::io::Error::other("not running"))?;
        self.turns += 1;
        self.cost += 0.001;
        let (tx, rx) = channel();
        let request = format!("perm-{}", self.turns);
        *self.answer.lock().unwrap() = Some((request.clone(), tx));
        let turn = Turn {
            n: self.turns,
            text: text.to_string(),
            cwd: self.cwd.clone(),
            request,
            cost: self.cost,
            step: self.step,
        };
        std::thread::spawn(move || turn.play(&sink, &rx));
        Ok(())
    }

    fn answer_permission(&mut self, request_id: &str, allow: bool, _input: &Value) -> std::io::Result<()> {
        let mut pending = self.answer.lock().unwrap();
        match pending.take() {
            Some((id, tx)) if id == request_id => {
                let _ = tx.send(allow);
                Ok(())
            }
            other => {
                *pending = other;
                Err(std::io::Error::other("no such permission prompt"))
            }
        }
    }

    fn interrupt(&mut self) -> std::io::Result<()> {
        // Refusing the pending prompt ends the turn early.
        if let Some((_, tx)) = self.answer.lock().unwrap().take() {
            let _ = tx.send(false);
        }
        Ok(())
    }

    fn stop(&mut self) {
        let _ = self.interrupt();
        if let Some(sink) = self.sink.take() {
            sink(Message::Closed);
        }
    }
}

struct Turn {
    n: u32,
    text: String,
    cwd: String,
    request: String,
    cost: f64,
    step: Duration,
}

impl Turn {
    fn play(self, sink: &Sink, answer: &Receiver<bool>) {
        let send = |e: Event| {
            if !self.step.is_zero() {
                std::thread::sleep(self.step);
            }
            sink(Message::Event(e));
        };
        let n = self.n;
        let tool = format!("tool-{n}");
        let agent = format!("agent-{n}");
        let path = Path::new(&self.cwd).join("notes.txt");
        let file_diff = FileDiff { path: path.clone(), created: false, hunks: vec![diff::text_hunk("old line", "new line")] };
        let input = json!({ "path": path, "old": "old line", "new": "new line" });

        send(Event::Status("thinking".into()));
        send(Event::TextStart);
        for word in ["Working ", "on: ", &self.text] {
            send(Event::TextDelta(word.to_string()));
        }
        send(Event::Text { text: format!("Working on: {}", self.text), parent: None });
        send(Event::ToolCall {
            id: tool.clone(),
            name: "edit".into(),
            summary: "notes.txt".into(),
            input: input.clone(),
            diff: Some(file_diff.clone()),
            parent: None,
        });
        send(Event::Permission {
            request_id: self.request.clone(),
            tool: "edit".into(),
            summary: "notes.txt".into(),
            description: Some("Change one line".into()),
            input,
        });
        let allowed = answer.recv().unwrap_or(false);
        send(Event::ToolResult {
            id: tool,
            output: if allowed { "changed".into() } else { "refused".into() },
            is_error: !allowed,
            diff: allowed.then_some(file_diff),
            file_changed: None,
            parent: None,
        });
        send(Event::ToolCall {
            id: agent.clone(),
            name: "subagent".into(),
            summary: "look around".into(),
            input: json!({ "description": "look around" }),
            diff: None,
            parent: None,
        });
        send(Event::SubagentStarted { id: agent.clone(), description: "look around".into(), kind: "helper".into() });
        send(Event::Text { text: "Looked around.".into(), parent: Some(agent.clone()) });
        send(Event::SubagentProgress {
            id: agent.clone(),
            current_tool: Some("read".into()),
            tokens: Some(120),
            tool_uses: Some(1),
            duration_ms: Some(50),
        });
        send(Event::ToolResult {
            id: agent.clone(),
            output: "nothing to report".into(),
            is_error: false,
            diff: None,
            file_changed: None,
            parent: None,
        });
        send(Event::SubagentEnded { id: agent, failed: false, summary: Some("done".into()), tokens: Some(120), duration_ms: Some(60) });
        send(Event::Text { text: if allowed { "Done.".into() } else { "Stopped.".into() }, parent: None });
        send(Event::TurnEnded {
            error: None,
            cost_total_usd: Some(self.cost),
            input_tokens: 10,
            output_tokens: 20,
            duration_ms: Some(100),
            session_id: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{Conversation, Item};
    use crate::host::LocalHost;

    fn run(allow: bool) -> Conversation {
        let backend = ScriptedBackend::default();
        let (tx, rx) = channel::<Message>();
        let tx = Mutex::new(tx);
        let mut agent = backend.new_agent();
        agent
            .start(Arc::new(LocalHost), &StartOptions { cwd: "/tmp/p".into(), ..Default::default() }, Arc::new(move |m| {
                let _ = tx.lock().unwrap().send(m);
            }))
            .unwrap();
        let mut chat = Conversation::new("/tmp/p");
        agent.send("hello").unwrap();
        loop {
            let Message::Event(e) = rx.recv_timeout(Duration::from_secs(5)).unwrap() else { continue };
            let done = matches!(e, Event::TurnEnded { .. });
            if let Event::Permission { request_id, .. } = &e {
                let id = request_id.clone();
                chat.apply(e);
                agent.answer_permission(&id, allow, &Value::Null).unwrap();
                continue;
            }
            chat.apply(e);
            if done {
                break;
            }
        }
        agent.stop();
        chat
    }

    #[test]
    fn plays_a_turn_through_the_neutral_model() {
        let chat = run(true);
        assert!(!chat.busy);
        assert_eq!(chat.model.as_deref(), Some("scripted"));
        assert_eq!(chat.agents.len(), 1);
        assert!(!chat.agents[0].running());
        assert!(chat.agents[0].items.iter().any(|i| matches!(i, Item::Assistant { .. })));
        let edit = chat.items.iter().find_map(|i| match i {
            Item::Tool(t) if t.name == "edit" => Some(t),
            _ => None,
        });
        assert!(edit.is_some_and(|t| t.done() && t.diff.is_some()));
        assert!((chat.total_cost_usd - 0.001).abs() < 1e-9);
    }

    #[test]
    fn a_refused_prompt_is_an_error_result() {
        let chat = run(false);
        let edit = chat.items.iter().find_map(|i| match i {
            Item::Tool(t) if t.name == "edit" => Some(t),
            _ => None,
        });
        assert!(edit.is_some_and(|t| t.is_error));
    }

    #[test]
    fn hides_what_it_does_not_have() {
        let caps = ScriptedBackend::default().capabilities();
        assert!(!caps.skills && !caps.earlier_sessions && !caps.fork && caps.models.is_empty());
    }
}
