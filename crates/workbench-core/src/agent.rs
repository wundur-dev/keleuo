//! The agent behind a chat session. The window talks to an [`Agent`]: start it, send a turn,
//! receive [`Message`]s, answer a permission prompt, interrupt, stop. Only Claude Code is
//! implemented ([`ClaudeCode`]); another CLI agent would map its own output onto the same
//! [`crate::stream::Event`]s.

use std::sync::Arc;

use serde_json::Value;

pub use crate::claude::Message;
use crate::claude::{Options, Process};
use crate::host::Host;
use crate::stream;

/// Where every message from the agent goes (called on a reader thread).
pub type Sink = Arc<dyn Fn(Message) + Send + Sync>;

/// What a session asks of its agent.
#[derive(Clone, Debug, Default)]
pub struct StartOptions {
    pub cwd: std::path::PathBuf,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    /// Continue this earlier conversation.
    pub resume: Option<String>,
    /// Continue it in a copy (the original keeps running elsewhere).
    pub fork: bool,
}

pub trait Agent: Send {
    /// Shown in the UI, e.g. "Claude Code".
    fn name(&self) -> &str;
    fn running(&self) -> bool;
    /// Start the agent process on `host`. Messages arrive through `sink`.
    fn start(&mut self, host: &dyn Host, options: &StartOptions, sink: Sink) -> anyhow::Result<()>;
    /// Send a user turn.
    fn send(&mut self, text: &str) -> std::io::Result<()>;
    /// Answer a permission prompt; `input` is the tool input to run with when allowed.
    fn answer_permission(
        &mut self,
        request_id: &str,
        allow: bool,
        input: &Value,
    ) -> std::io::Result<()>;
    /// Stop the current turn, keeping the process.
    fn interrupt(&mut self) -> std::io::Result<()>;
    /// End the process.
    fn stop(&mut self);
}

/// Claude Code over `stream-json`.
#[derive(Default)]
pub struct ClaudeCode {
    process: Option<Process>,
}

impl ClaudeCode {
    pub fn new() -> Self {
        Self::default()
    }

    fn process(&mut self) -> std::io::Result<&mut Process> {
        self.process
            .as_mut()
            .ok_or_else(|| std::io::Error::other("Claude Code is not running"))
    }
}

impl Agent for ClaudeCode {
    fn name(&self) -> &str {
        "Claude Code"
    }

    fn running(&self) -> bool {
        self.process.is_some()
    }

    fn start(&mut self, host: &dyn Host, options: &StartOptions, sink: Sink) -> anyhow::Result<()> {
        let spec = host.claude_command(&Options {
            program: None,
            cwd: options.cwd.clone(),
            model: options.model.clone(),
            permission_mode: options.permission_mode.clone(),
            resume: options.resume.clone(),
            fork: options.fork,
        });
        self.process = Some(Process::spawn(&spec, move |m| sink(m))?);
        Ok(())
    }

    fn send(&mut self, text: &str) -> std::io::Result<()> {
        self.process()?.send_user(text)
    }

    fn answer_permission(
        &mut self,
        request_id: &str,
        allow: bool,
        input: &Value,
    ) -> std::io::Result<()> {
        self.process()?
            .send_line(&stream::permission_response(request_id, allow, input))
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
    use std::sync::Mutex;

    /// A fake `claude` that echoes every stdin line back as an `Other` event, through the
    /// trait object.
    #[test]
    fn claude_code_through_the_trait() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("wb-agent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("claude");
        std::fs::write(
            &script,
            "#!/bin/sh\nwhile read line; do echo \"$line\"; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        // A host whose claude is the fake one.
        struct Fake(std::path::PathBuf);
        impl Host for Fake {
            fn label(&self) -> String {
                "fake".into()
            }
            fn is_remote(&self) -> bool {
                false
            }
            fn home_dir(&self) -> Option<std::path::PathBuf> {
                None
            }
            fn read(&self, p: &std::path::Path) -> std::io::Result<Vec<u8>> {
                std::fs::read(p)
            }
            fn write(&self, p: &std::path::Path, c: &[u8]) -> std::io::Result<()> {
                std::fs::write(p, c)
            }
            fn create_dir_all(&self, p: &std::path::Path) -> std::io::Result<()> {
                std::fs::create_dir_all(p)
            }
            fn exists(&self, p: &std::path::Path) -> bool {
                p.exists()
            }
            fn is_file(&self, p: &std::path::Path) -> bool {
                p.is_file()
            }
            fn read_dir(&self, _: &std::path::Path) -> std::io::Result<Vec<crate::host::DirEntry>> {
                Ok(vec![])
            }
            fn list_files(&self, _: &std::path::Path, _: usize) -> Vec<(std::path::PathBuf, bool)> {
                vec![]
            }
            fn watch(
                &self,
                _: &std::path::Path,
                _: std::sync::mpsc::Sender<crate::host::Change>,
            ) -> Option<crate::host::Watch> {
                None
            }
            fn shell_command(&self, cwd: &std::path::Path) -> crate::host::CommandSpec {
                crate::host::LocalHost.shell_command(cwd)
            }
            fn claude_command(&self, o: &Options) -> crate::host::CommandSpec {
                crate::host::CommandSpec {
                    program: self.0.clone(),
                    args: o.args(),
                    cwd: Some(o.cwd.clone()),
                }
            }
        }

        let got = Arc::new(Mutex::new(Vec::new()));
        let sink_got = got.clone();
        let mut agent: Box<dyn Agent> = Box::new(ClaudeCode::new());
        assert!(!agent.running());
        agent
            .start(
                &Fake(script),
                &StartOptions {
                    cwd: dir.clone(),
                    ..Default::default()
                },
                Arc::new(move |m| sink_got.lock().unwrap().push(m)),
            )
            .unwrap();
        assert!(agent.running());
        agent.send("hi").unwrap();
        agent.answer_permission("r1", false, &Value::Null).unwrap();
        let start = std::time::Instant::now();
        while got.lock().unwrap().len() < 2 && start.elapsed().as_secs() < 5 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        agent.stop();
        assert!(!agent.running());
        let got = got.lock().unwrap();
        // The user turn comes back as a `user` line, the answer as a `control_response`.
        assert!(
            matches!(&got[0], Message::Event(stream::Event::UserText(t)) if t == "hi"),
            "{:?}",
            got[0]
        );
        assert!(
            matches!(&got[1], Message::Event(stream::Event::Other(t)) if t == "control_response")
        );
        assert_eq!(agent.name(), "Claude Code");
        let _ = std::fs::remove_dir_all(dir);
    }
}
