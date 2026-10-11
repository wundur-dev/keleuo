//! The `claude` subprocess: started in the project folder with structured input and output,
//! read on background threads, its lines parsed into [`Event`]s.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::Arc;

use crate::host::CommandSpec;

use crate::stream::{self, Event};

/// How to start Claude Code.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The `claude` program; found on PATH and in the usual install places when `None`.
    pub program: Option<PathBuf>,
    pub cwd: PathBuf,
    /// `--model`, when not the user's default.
    pub model: Option<String>,
    /// `--permission-mode`; `None` keeps Claude's default (it asks).
    pub permission_mode: Option<String>,
    /// `--resume <session id>`: continue an earlier conversation.
    pub resume: Option<String>,
}

impl Options {
    /// The command line arguments.
    pub fn args(&self) -> Vec<String> {
        let mut args: Vec<String> = [
            "-p",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            // Permission prompts come to us as control requests instead of being denied.
            "--permission-prompt-tool",
            "stdio",
        ]
        .map(String::from)
        .into();
        if let Some(model) = &self.model {
            args.extend(["--model".into(), model.clone()]);
        }
        if let Some(mode) = &self.permission_mode {
            args.extend(["--permission-mode".into(), mode.clone()]);
        }
        if let Some(id) = &self.resume {
            args.extend(["--resume".into(), id.clone()]);
        }
        args
    }
}

/// What the reader threads report.
#[derive(Debug)]
pub enum Message {
    Event(Event),
    /// A line Claude Code wrote to stderr.
    Stderr(String),
    /// stdout closed: the process has ended (or is about to).
    Closed,
}

/// A running `claude`.
pub struct Process {
    child: Child,
    stdin: Option<ChildStdin>,
    next_request: u64,
}

impl Process {
    /// Start `spec` (from [`crate::host::Host::claude_command`]). Every message is handed to
    /// `sink` on a reader thread; the window turns that into a wake-up, so nothing polls.
    pub fn spawn(
        spec: &CommandSpec,
        sink: impl Fn(Message) + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let mut child = spec
            .to_command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| anyhow::anyhow!("could not start {}: {e}", spec.program.display()))?;
        let sink: Arc<dyn Fn(Message) + Send + Sync> = Arc::new(sink);
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        read_lines(
            stdout,
            sink.clone(),
            |line| stream::parse_line(&line).map(Message::Event),
            true,
        );
        read_lines(stderr, sink, |line| Some(Message::Stderr(line)), false);
        Ok(Self {
            stdin: child.stdin.take(),
            child,
            next_request: 0,
        })
    }

    /// Write one JSON line to Claude's stdin.
    pub fn send_line(&mut self, line: &str) -> std::io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| std::io::Error::other("stdin closed"))?;
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }

    pub fn send_user(&mut self, text: &str) -> std::io::Result<()> {
        self.send_line(&stream::user_message(text))
    }

    /// Ask Claude to stop the current turn.
    pub fn interrupt(&mut self) -> std::io::Result<()> {
        self.next_request += 1;
        let id = format!("workbench-{}", self.next_request);
        self.send_line(&stream::interrupt_request(&id))
    }

    /// The process exit status, if it has ended.
    pub fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn kill(&mut self) {
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.kill();
    }
}

fn read_lines<R: std::io::Read + Send + 'static>(
    reader: R,
    sink: Arc<dyn Fn(Message) + Send + Sync>,
    map: impl Fn(String) -> Option<Message> + Send + 'static,
    report_close: bool,
) {
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else { break };
            if let Some(msg) = map(line) {
                sink(msg);
            }
        }
        if report_close {
            sink(Message::Closed);
        }
    });
}

/// Find `claude` on PATH or where its installer puts it (a window started from the Finder does
/// not get the shell's PATH).
pub fn find_claude() -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".claude/local"));
        dirs.push(home.join(".npm-global/bin"));
    }
    dirs.push("/opt/homebrew/bin".into());
    dirs.push("/usr/local/bin".into());
    dirs.into_iter().map(|d| d.join(exe)).find(|p| is_file(p))
}

fn is_file(p: &Path) -> bool {
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_default_and_options() {
        let base = Options {
            cwd: "/p".into(),
            ..Default::default()
        }
        .args();
        assert!(base.starts_with(&["-p".to_string()]));
        assert!(
            base.windows(2)
                .any(|w| w == ["--permission-prompt-tool", "stdio"])
        );
        assert!(
            !base
                .iter()
                .any(|a| a == "--permission-mode" || a.contains("dangerously"))
        );
        let full = Options {
            cwd: "/p".into(),
            model: Some("haiku".into()),
            permission_mode: Some("acceptEdits".into()),
            resume: Some("abc".into()),
            ..Default::default()
        }
        .args();
        assert!(full.windows(2).any(|w| w == ["--model", "haiku"]));
        assert!(
            full.windows(2)
                .any(|w| w == ["--permission-mode", "acceptEdits"])
        );
        assert!(full.windows(2).any(|w| w == ["--resume", "abc"]));
    }

    /// A fake `claude` (a shell script printing a recorded stream) drives the real process code.
    #[cfg(unix)]
    #[test]
    fn spawn_reads_events_from_a_fake_claude() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("wb-fake-claude-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("claude");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/edit_turn.jsonl");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nread line\ncat '{}'\necho oops >&2\n",
                fixture.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let options = Options {
            program: Some(script),
            cwd: dir.clone(),
            ..Default::default()
        };
        let spec = crate::host::Host::claude_command(&crate::host::LocalHost, &options);
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let mut process = Process::spawn(&spec, move |m| {
            let _ = tx.lock().unwrap().send(m);
        })
        .unwrap();
        process.send_user("hello").unwrap();
        let mut events = Vec::new();
        let mut closed = false;
        let mut stderr = Vec::new();
        let start = std::time::Instant::now();
        while !(closed && !stderr.is_empty()) && start.elapsed().as_secs() < 10 {
            for m in rx.try_iter() {
                match m {
                    Message::Event(e) => events.push(e),
                    Message::Stderr(s) => stderr.push(s),
                    Message::Closed => closed = true,
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(closed);
        assert_eq!(stderr, ["oops"]);
        assert!(matches!(events.first(), Some(Event::Init { .. })));
        assert!(matches!(events.last(), Some(Event::Result { .. })));
        let _ = std::fs::remove_dir_all(dir);
    }
}
