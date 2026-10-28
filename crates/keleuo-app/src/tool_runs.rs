//! Running the tools the person made from command-line programs ([`keleuo_core::cli_tools`]),
//! when an agent calls one through `keleuo-mcp`. The window runs it, not the agent: in the
//! session's folder, with its secrets from the key store (the agent never sees them), its
//! writes kept to the folder and the ones the tool names (the write sandbox), stopped at its
//! timeout. A tool set to ask shows a card in the session's chat first (Allow, Always for this
//! session, Deny); the agent's call waits for it.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use base64::Engine as _;
use gpui_kit::*;
use keleuo_core::cli_tools::{self, Output, Tool};
use keleuo_core::packs::{self, Pack, PackTool};
use keleuo_core::conversation::SendState;
use serde_json::{Value, json};

use crate::agent_session::AgentSession;
use crate::hooks::Reply;
use crate::messaging::Decision;

/// Output kept, at most (a tool printing more is cut there).
const MAX_OUTPUT: usize = 100_000;
/// Images sent back, at most, and their size each.
const MAX_IMAGES: usize = 4;
const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;

/// What a run runs: a command (the person's own tool), or a pack's script.
enum Job {
    Command(Vec<String>),
    Pack(Box<(Pack, PackTool)>, Value),
}

/// A run waiting for the person.
struct Pending {
    session: WeakEntity<AgentSession>,
    tool: Tool,
    job: Job,
    reply: Option<Reply>,
}

#[derive(Default)]
struct Runs {
    next: u64,
    pending: HashMap<u64, Pending>,
    /// Tools a session may run without asking (its entity, the tool's name).
    always: HashSet<(EntityId, String)>,
}

impl Global for Runs {}

/// The tools the session's agent gets: the person's own, then the packs' (those with the keys
/// they need; a name the person's own tools have already is theirs).
fn tools_here(session: &Entity<AgentSession>, cx: &App) -> Vec<Tool> {
    let s = session.read(cx);
    if s.host_is_remote() {
        return Vec::new();
    }
    let settings = cx.global::<crate::AppSettings>().0.borrow();
    let mut tools = cli_tools::tools_for(&settings.cli_tools, &settings.folder_tools, s.cwd());
    for (pack, tool) in pack_tools(cx) {
        if !tools.iter().any(|t| t.name == tool.name) {
            tools.push(pack.as_tool(&tool));
        }
    }
    tools
}

/// The installed, switched-on packs' tools that have the keys they need.
fn pack_tools(cx: &App) -> Vec<(Pack, PackTool)> {
    let Some(dir) = packs::dir() else { return Vec::new() };
    let off = cx.global::<crate::AppSettings>().0.borrow().packs_off.clone();
    packs::installed(&dir)
        .into_iter()
        .filter(|p| !off.contains(&p.manifest.id))
        .flat_map(|p| {
            let has = |name: &str| keleuo_core::secrets::kept(&p.secret_account(name));
            let ready: Vec<PackTool> = p.manifest.tools.iter().filter(|t| Pack::ready(t, &has)).cloned().collect();
            ready.into_iter().map(move |t| (p.clone(), t))
        })
        .collect()
}

/// The tools, as `keleuo-mcp` lists them.
pub fn list(session: &Entity<AgentSession>, cx: &App) -> Value {
    json!({ "tools": tools_here(session, cx).iter().map(cli_tools::describe).collect::<Vec<_>>() })
}

fn answer(reply: Option<Reply>, text: String, error: bool, images: Vec<Value>) {
    if let Some(reply) = reply {
        reply.send(&json!({ "text": text, "error": error, "images": images }).to_string());
    }
}

/// An agent's call of tool `name`: run (or asked about first), answered on `reply`.
pub fn call(session: Entity<AgentSession>, name: &str, args: Value, reply: Option<Reply>, cx: &mut App) {
    let Some(tool) = tools_here(&session, cx).into_iter().find(|t| t.name == name) else {
        return answer(reply, format!("Keleuo has no tool {name}."), true, Vec::new());
    };
    // The person's own tool of that name, else a pack's.
    let own = {
        let settings = cx.global::<crate::AppSettings>().0.borrow();
        cli_tools::tools_for(&settings.cli_tools, &settings.folder_tools, session.read(cx).cwd()).iter().any(|t| t.name == name)
    };
    let (job, shown) = if own {
        match cli_tools::argv(&tool, &args) {
            Ok(argv) => {
                let shown = argv.iter().map(|a| keleuo_core::host::shell_quote(a)).collect::<Vec<_>>().join(" ");
                (Job::Command(argv), shown)
            }
            Err(why) => return answer(reply, format!("Not run: {why}."), true, Vec::new()),
        }
    } else {
        let Some(found) = pack_tools(cx).into_iter().find(|(_, t)| t.name == name) else {
            return answer(reply, format!("Keleuo has no tool {name}."), true, Vec::new());
        };
        let shown = format!("{} ({}) {}{}", found.1.name, found.0.manifest.name, args, if found.1.cost.is_empty() { String::new() } else { format!(" · costs {}", found.1.cost) });
        (Job::Pack(Box::new(found), args), shown)
    };
    let key = (session.entity_id(), tool.name.clone());
    if tool.ask && !cx.try_global::<Runs>().is_some_and(|r| r.always.contains(&key)) {
        let runs = cx.default_global::<Runs>();
        runs.next += 1;
        let id = runs.next;
        runs.pending.insert(id, Pending { session: session.downgrade(), tool: tool.clone(), job, reply });
        session.update(cx, |s, cx| s.note_tool_run(id, &tool.name, &shown, SendState::Asking, cx));
        return;
    }
    start(&session, tool, job, reply, cx);
}

/// The person's answer to run `id`.
pub fn decide(id: u64, decision: Decision, cx: &mut App) {
    let Some(p) = cx.default_global::<Runs>().pending.remove(&id) else { return };
    let Some(session) = p.session.upgrade() else { return answer(p.reply, "The session closed.".into(), true, Vec::new()) };
    if decision == Decision::Deny {
        session.update(cx, |s, cx| s.set_tool_run(id, SendState::Denied, cx));
        return answer(p.reply, "The person declined to run it.".into(), true, Vec::new());
    }
    if decision == Decision::Always {
        cx.default_global::<Runs>().always.insert((session.entity_id(), p.tool.name.clone()));
    }
    session.update(cx, |s, cx| s.set_tool_run(id, SendState::Sent, cx));
    start(&session, p.tool, p.job, p.reply, cx);
}

/// Run it off the UI thread and answer when it ends.
fn start(session: &Entity<AgentSession>, tool: Tool, job: Job, reply: Option<Reply>, cx: &mut App) {
    let cwd = session.read(cx).cwd().to_path_buf();
    let argv = match job {
        Job::Command(argv) => argv,
        Job::Pack(found, args) => {
            let (pack, ptool) = *found;
            let settings = cx.global::<crate::AppSettings>().0.borrow().pack_settings.get(&pack.manifest.id).cloned().unwrap_or_default();
            // A page it made (a model viewer) opens in the session's web panel once it is done.
            let (shown, show_rx) = async_channel::bounded::<std::path::PathBuf>(1);
            let weak = session.downgrade();
            cx.spawn(async move |cx: &mut AsyncApp| {
                if let Ok(path) = show_rx.recv().await {
                    let _ = weak.update(cx, |_, cx| cx.emit(crate::agent_session::SessionEvent::ShowPage(path)));
                }
            })
            .detach();
            std::thread::spawn(move || {
                let secret = |name: &str| keleuo_core::secrets::get(&pack.secret_account(name));
                let grant = packs::Grant { cwd: &cwd, secret: &secret, settings: &settings, extra_hosts: Vec::new(), allow_http: false };
                let out = packs::run(&pack, &ptool, &args, &grant);
                let mut text = out.text;
                if !out.files.is_empty() {
                    let names: Vec<String> = out.files.iter().map(|f| f.strip_prefix(&cwd).unwrap_or(f).display().to_string()).collect();
                    text.push_str(&format!("\nFiles: {}", names.join(", ")));
                }
                if let Some(page) = out.show.filter(|_| !out.failed) {
                    let _ = shown.try_send(page);
                }
                answer(reply, text, out.failed, images_as_content(&out.images));
            });
            return;
        }
    };
    let env: Vec<(String, String)> = tool
        .secrets
        .iter()
        .filter_map(|n| keleuo_core::secrets::get(&cli_tools::secret_account(&tool.name, n)).map(|v| (n.clone(), v)))
        .collect();
    let policy = crate::prefs(cx).sandbox_writes.then(|| keleuo_core::sandbox::policy_for(&cwd, &[], &tool.writes));
    std::thread::spawn(move || {
        let (text, error, images) = run(&tool, argv, &cwd, env, policy.as_ref());
        answer(reply, text, error, images);
    });
}

/// Images as MCP image content (the first few, each not too big).
fn images_as_content(paths: &[PathBuf]) -> Vec<Value> {
    paths
        .iter()
        .filter(|p| p.metadata().is_ok_and(|m| m.len() <= MAX_IMAGE_BYTES))
        .take(MAX_IMAGES)
        .filter_map(|p| {
            let bytes = std::fs::read(p).ok()?;
            let mime = match p.extension().and_then(|e| e.to_str()).map(str::to_lowercase).as_deref() {
                Some("jpg" | "jpeg") => "image/jpeg",
                Some("webp") => "image/webp",
                Some("gif") => "image/gif",
                _ => "image/png",
            };
            Some(json!({ "data": base64::engine::general_purpose::STANDARD.encode(bytes), "mimeType": mime }))
        })
        .collect()
}

/// `~/` at a word's start as the home folder (no shell does it).
fn home(word: String) -> String {
    match (word.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(h)) => h.join(rest).to_string_lossy().to_string(),
        _ => word,
    }
}

/// Run the command: its output (and whether it failed), and its images when it makes them.
fn run(tool: &Tool, argv: Vec<String>, cwd: &Path, env: Vec<(String, String)>, policy: Option<&keleuo_core::sandbox::WritePolicy>) -> (String, bool, Vec<Value>) {
    let mut words = argv.into_iter().map(home);
    let program = words.next().unwrap_or_default();
    let spec = keleuo_core::host::CommandSpec { program: PathBuf::from(&program), args: words.collect(), cwd: Some(cwd.to_path_buf()), env };
    let spec = keleuo_core::sandbox::apply(spec, policy);
    let mut child = match spec.to_command().stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
        Ok(c) => c,
        Err(e) => return (format!("Could not start {program}: {e}"), true, Vec::new()),
    };
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut p) = pipe {
                let _ = p.by_ref().take(MAX_OUTPUT as u64).read_to_string(&mut text);
            }
            text
        })
    };
    let out = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let deadline = std::time::Instant::now() + tool.timeout();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let (stdout, stderr) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    let Some(status) = status else {
        return (format!("Stopped after {} seconds.\n{stdout}{stderr}", tool.timeout().as_secs()), true, Vec::new());
    };
    let failed = !status.success();
    let mut text = stdout.trim_end().to_string();
    if failed || text.is_empty() {
        if !stderr.trim().is_empty() {
            text = format!("{text}\n{}", stderr.trim_end()).trim().to_string();
        }
        if failed {
            text = format!("{text}\n(exit status {})", status.code().map_or("killed".into(), |c| c.to_string())).trim().to_string();
        }
    }
    let images = if tool.output == Output::Images && !failed { images_as_content(&cli_tools::images_in(&stdout, cwd)) } else { Vec::new() };
    (if text.is_empty() { "(no output)".into() } else { text }, failed, images)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn a_tool_runs_without_a_shell_and_says_how_it_ended() {
        let tool = Tool { name: "echo".into(), command: vec!["/bin/echo".into(), "{word}".into()], enabled: true, ..Default::default() };
        let (text, failed, _) = run(&tool, vec!["/bin/echo".into(), "$HOME; ls".into()], &std::env::temp_dir(), Vec::new(), None);
        assert_eq!((text.as_str(), failed), ("$HOME; ls", false), "the argument is one word, never read by a shell");
        let (text, failed, _) = run(&tool, vec!["/bin/sh".into(), "-c".into(), "echo oops >&2; exit 3".into()], &std::env::temp_dir(), Vec::new(), None);
        assert!(failed && text.contains("oops") && text.contains("exit status 3"), "{text}");
    }

    #[::core::prelude::v1::test]
    fn a_tool_that_runs_too_long_is_stopped() {
        let tool = Tool { name: "slow".into(), timeout_secs: 1, enabled: true, ..Default::default() };
        let (text, failed, _) = run(&tool, vec!["/bin/sleep".into(), "5".into()], &std::env::temp_dir(), Vec::new(), None);
        assert!(failed && text.starts_with("Stopped after 1 seconds"), "{text}");
    }
}
