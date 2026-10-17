//! Messages between sessions: an agent in one session asks Keleuo to reach another, through
//! the tool server it was started with (`keleuo-mcp`, its calls routed here by `hooks`). It
//! can list the sessions open in the window and send one of them a message, by name.
//!
//! The person stays in charge: a message shows in the sender's chat as a card, and goes only
//! once they allow it (Allow, Always for these two, Deny). "Always" lets that pair talk without
//! asking, but only so fast: past [`MAX_FREE`] messages between the same two sessions within
//! [`FREE_WINDOW`], it asks again, so two agents cannot keep each other going unattended.
//!
//! The receiving session shows the message as from the other session, never as the person's,
//! and its agent gets it as a new turn (after the one it is in), marked the same way, with how
//! to answer.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui_kit::*;
use keleuo_core::conversation::SendState;
use serde_json::{Value, json};

use crate::agent_session::AgentSession;
use crate::hooks::Call;

/// Messages one session may send another without asking, once allowed always...
const MAX_FREE: usize = 8;
/// ...within this long.
const FREE_WINDOW: Duration = Duration::from_secs(10 * 60);

/// A session another can reach: its place in the list (its number), its name, its agent and
/// folder.
pub struct Peer {
    pub number: usize,
    pub name: String,
    pub agent: String,
    pub folder: std::path::PathBuf,
    pub session: Entity<AgentSession>,
}

/// Where a message goes: a session in the window, or a new reviewer session.
enum Target {
    Session { to: WeakEntity<AgentSession>, to_number: usize },
    /// A new read-only session of `backend` in the sender's folder, asked for a review.
    Reviewer { backend: std::sync::Arc<dyn keleuo_core::agent::Backend> },
}

/// A message waiting for the person.
struct Pending {
    from: Entity<AgentSession>,
    from_number: usize,
    from_name: String,
    target: Target,
    text: String,
}

/// The window's messages between sessions.
#[derive(Default)]
struct Messaging {
    next: u64,
    pending: HashMap<u64, Pending>,
    /// Pairs (from, to: session numbers) allowed to send without asking.
    always: HashSet<(usize, usize)>,
    /// When each pair last sent, for the limit.
    sent: HashMap<(usize, usize), Vec<Instant>>,
}

impl Global for Messaging {}

/// The person's answer on a message's card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Always,
    Deny,
}

/// The sessions in the window, from the window's shell.
fn peers(cx: &App) -> Vec<Peer> {
    let shell = cx.try_global::<crate::control::ControlTargets>().and_then(|t| t.workspace.as_ref().map(|(_, s)| s.clone()));
    shell.and_then(|s| s.upgrade()).map(|s| s.read(cx).directory(cx)).unwrap_or_default()
}

/// An agent's tool call: answered on the call's connection.
pub fn tool_call(session: Entity<AgentSession>, call: Call, cx: &mut App) {
    // `keleuo-mcp` asking which of the person's tools the agent gets.
    if call.payload.get("list").is_some_and(|v| v == &json!(true)) {
        if let Some(reply) = call.reply {
            reply.send(&crate::tool_runs::list(&session, cx).to_string());
        }
        return;
    }
    let tool = call.payload.get("tool").and_then(Value::as_str).unwrap_or_default().to_string();
    let args = call.payload.get("arguments").cloned().unwrap_or(json!({}));
    let (text, error) = match tool.as_str() {
        "list_sessions" => (list(&session, cx), false),
        "send_message" => send(&session, &args, cx),
        "ask_for_review" => review(&session, &args, cx),
        // The person's tools answer when their run ends.
        _ => return crate::tool_runs::call(session, &tool, args, call.reply, cx),
    };
    if let Some(reply) = call.reply {
        reply.send(&json!({ "text": text, "error": error }).to_string());
    }
}

fn list(me: &Entity<AgentSession>, cx: &App) -> String {
    let peers = peers(cx);
    let mine = peers.iter().find(|p| &p.session == me).map(|p| p.name.clone()).unwrap_or_default();
    let others: Vec<String> = peers
        .iter()
        .filter(|p| &p.session != me)
        .map(|p| format!("- {} ({}, in {})", p.name, p.agent, p.folder.display()))
        .collect();
    if others.is_empty() {
        return format!("No other sessions are open in Keleuo. (This one is \"{mine}\".)");
    }
    format!("Sessions open in Keleuo (this one is \"{mine}\"):\n{}", others.join("\n"))
}

/// The session named `name` (its name as shown, ignoring case; else its folder's name), or why
/// none can be chosen.
fn resolve<'a>(name: &str, me: &Entity<AgentSession>, peers: &'a [Peer]) -> Result<&'a Peer, String> {
    let name = name.trim().trim_matches('"');
    let others: Vec<&Peer> = peers.iter().filter(|p| &p.session != me).collect();
    let by = |f: &dyn Fn(&Peer) -> bool| others.iter().copied().filter(|p| f(p)).collect::<Vec<_>>();
    let mut found = by(&|p| p.name.eq_ignore_ascii_case(name));
    if found.is_empty() {
        found = by(&|p| p.folder.file_name().is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(name)));
    }
    let choices = || others.iter().map(|p| format!("\"{}\"", p.name)).collect::<Vec<_>>().join(", ");
    match found.as_slice() {
        [one] => Ok(one),
        [] if peers.iter().any(|p| &p.session == me && p.name.eq_ignore_ascii_case(name)) => {
            Err("That is this session; name another one.".into())
        }
        [] => Err(format!("No session is named \"{name}\". The sessions open: {}.", choices())),
        _ => Err(format!("More than one session is called \"{name}\"; the person should rename one. The sessions open: {}.", choices())),
    }
}

fn send(me: &Entity<AgentSession>, args: &Value, cx: &mut App) -> (String, bool) {
    let to = args.get("to").and_then(Value::as_str).unwrap_or_default();
    let text = args.get("message").and_then(Value::as_str).unwrap_or_default().trim().to_string();
    if text.is_empty() {
        return ("The message is empty.".into(), true);
    }
    let peers = peers(cx);
    let Some(from) = peers.iter().find(|p| &p.session == me) else { return ("This session is not in the window's list.".into(), true) };
    let target = match resolve(to, me, &peers) {
        Ok(t) => t,
        Err(why) => return (why, true),
    };
    let pair = (from.number, target.number);
    let state = cx.default_global::<Messaging>();
    state.next += 1;
    let id = state.next;
    let recent = state.sent.get(&pair).map_or(0, |times| times.iter().filter(|t| t.elapsed() < FREE_WINDOW).count());
    let free = state.always.contains(&pair) && recent < MAX_FREE;
    let pending = Pending {
        from: me.clone(),
        from_number: from.number,
        from_name: from.name.clone(),
        target: Target::Session { to: target.session.downgrade(), to_number: target.number },
        text: text.clone(),
    };
    let name = target.name.clone();
    if free {
        let card = me.clone();
        card.update(cx, |s, cx| s.note_outgoing(id, &name, &text, SendState::Sent, cx));
        deliver(pending, cx);
        (format!("Sent to \"{name}\". If it answers, its answer comes to this session as a new message."), false)
    } else {
        cx.default_global::<Messaging>().pending.insert(id, pending);
        me.update(cx, |s, cx| s.note_outgoing(id, &name, &text, SendState::Asking, cx));
        let why = if state_allows_always(pair, cx) { " (it has sent many messages in a short time, so the person is asked again)" } else { "" };
        (
            format!(
                "The person is asked to allow this message to \"{name}\"{why}. It goes as soon as they do; there is no need to wait for it or send it again."
            ),
            false,
        )
    }
}

fn state_allows_always(pair: (usize, usize), cx: &App) -> bool {
    cx.try_global::<Messaging>().is_some_and(|m| m.always.contains(&pair))
}

/// A review: the person is asked to allow a new read-only session of the agent asked for (or
/// another than this one's) to review, with `request`.
fn review(me: &Entity<AgentSession>, args: &Value, cx: &mut App) -> (String, bool) {
    let request = args.get("request").and_then(Value::as_str).unwrap_or_default().trim().to_string();
    if request.is_empty() {
        return ("Say what to review.".into(), true);
    }
    let peers = peers(cx);
    let Some(from) = peers.iter().find(|p| &p.session == me) else { return ("This session is not in the window's list.".into(), true) };
    let mine = me.read(cx).backend.id().to_string();
    // The real agents (the scripted test one only when asked for by name).
    let all = crate::backends::all(cx);
    let asked = args.get("agent").and_then(Value::as_str).map(str::to_lowercase).filter(|a| !a.trim().is_empty());
    let backend = match &asked {
        Some(a) => all.iter().find(|b| b.name().to_lowercase().contains(a.trim()) || b.id().contains(a.trim())).cloned(),
        None => all.iter().filter(|b| b.id() != "scripted").find(|b| b.id() != mine).or_else(|| all.first()).cloned(),
    };
    let Some(backend) = backend else {
        let names = all.iter().filter(|b| b.id() != "scripted").map(|b| format!("\"{}\"", b.name())).collect::<Vec<_>>().join(", ");
        return (format!("Keleuo has no agent called that. It has: {names}."), true);
    };
    let state = cx.default_global::<Messaging>();
    state.next += 1;
    let id = state.next;
    let to = format!("a new {} reviewer (read-only)", backend.name());
    state.pending.insert(id, Pending { from: me.clone(), from_number: from.number, from_name: from.name.clone(), target: Target::Reviewer { backend }, text: request.clone() });
    me.update(cx, |s, cx| s.note_outgoing(id, &to, &request, SendState::Asking, cx));
    (
        format!("The person is asked to allow {to} to review this. Its review comes to this session as a new message; finish your turn rather than waiting for it."),
        false,
    )
}

/// The person's answer to message `id`, from its card.
pub fn decide(id: u64, decision: Decision, cx: &mut App) {
    let Some(pending) = cx.default_global::<Messaging>().pending.remove(&id) else { return };
    let from = pending.from.clone();
    if decision == Decision::Deny {
        from.update(cx, |s, cx| s.set_outgoing(id, SendState::Denied, cx));
        return;
    }
    match &pending.target {
        Target::Session { to, to_number } => {
            if decision == Decision::Always {
                cx.default_global::<Messaging>().always.insert((pending.from_number, *to_number));
            }
            let state = if to.upgrade().is_some() { SendState::Sent } else { SendState::Failed("that session has closed".into()) };
            from.update(cx, |s, cx| s.set_outgoing(id, state, cx));
            deliver(pending, cx);
        }
        Target::Reviewer { backend } => {
            // After the click that allowed it, which still holds the window the reviewer opens in.
            let backend = backend.clone();
            cx.defer(move |cx| {
                let state = match open_reviewer(&pending, backend, decision, cx) {
                    Ok(()) => SendState::Sent,
                    Err(why) => SendState::Failed(why),
                };
                from.update(cx, |s, cx| s.set_outgoing(id, state, cx));
            });
        }
    }
}

/// Open the reviewer the person allowed: a new read-only session in the sender's folder, named
/// for it, given the request; its review may come back without asking (and, `Always`, more).
fn open_reviewer(pending: &Pending, backend: std::sync::Arc<dyn keleuo_core::agent::Backend>, decision: Decision, cx: &mut App) -> Result<(), String> {
    let (handle, shell) = cx
        .try_global::<crate::control::ControlTargets>()
        .and_then(|t| t.workspace.clone())
        .ok_or("the window is not there")?;
    let folder = pending.from.read(cx).cwd().to_path_buf();
    let name = format!("Review for {}", pending.from_name);
    let opened = handle
        .update(cx, |_, window, cx| shell.update(cx, |s, cx| s.open_reviewer(folder, backend, &name, window, cx)))
        .map_err(|_| "the window is not there".to_string())?
        .map_err(|_| "the window is not there".to_string())?
        .ok_or("the window is not there")?;
    let (number, reviewer) = opened;
    let m = cx.default_global::<Messaging>();
    // Its answer is what the person allowed; "always": the two may go on talking.
    m.always.insert((number, pending.from_number));
    if decision == Decision::Always {
        m.always.insert((pending.from_number, number));
    }
    let text = format!(
        "Please review, as a second opinion. You are in a read-only session: read files and run read-only commands, but do not change anything (writes to this folder are blocked).\n\n{}\n\nWhen you are done, send your review to \"{}\" with the keleuo send_message tool.",
        pending.text, pending.from_name
    );
    let from = pending.from_name.clone();
    reviewer.update(cx, |s, cx| s.receive_message(&from, &text, cx));
    Ok(())
}

/// Whether sessions `a` and `b` may message each other without asking (either way).
fn always_between(a: usize, b: usize, cx: &App) -> bool {
    cx.try_global::<Messaging>().is_some_and(|m| m.always.contains(&(a, b)) || m.always.contains(&(b, a)))
}

/// The sessions `me` may message, or be messaged by, without asking: `(me, other, name)` by
/// session number, for the chat's menu (to ask again).
pub fn allowed_with(me: &Entity<AgentSession>, cx: &App) -> Vec<(usize, usize, String)> {
    let peers = peers(cx);
    let Some(mine) = peers.iter().find(|p| &p.session == me).map(|p| p.number) else { return Vec::new() };
    peers
        .iter()
        .filter(|p| p.number != mine && always_between(mine, p.number, cx))
        .map(|p| (mine, p.number, p.name.clone()))
        .collect()
}

/// Ask again before messages between sessions `a` and `b` (either way).
pub fn forget_always(a: usize, b: usize, cx: &mut App) {
    let m = cx.default_global::<Messaging>();
    m.always.remove(&(a, b));
    m.always.remove(&(b, a));
}

fn deliver(pending: Pending, cx: &mut App) {
    let Target::Session { to, to_number } = &pending.target else { return };
    let Some(to) = to.upgrade() else { return };
    cx.default_global::<Messaging>().sent.entry((pending.from_number, *to_number)).or_default().push(Instant::now());
    let (from, text) = (pending.from_name, pending.text);
    to.update(cx, |s, cx| s.receive_message(&from, &text, cx));
}
