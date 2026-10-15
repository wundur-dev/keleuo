//! Cursor Agent in Chat over ACP (`cursor-agent acp`): one process per session, kept between
//! messages. What ACP standardises goes through [`crate::acp`]; Cursor's own extensions are
//! handled here in full: `cursor/ask_question` is a question card, its modes are its own, and
//! its models are the ones it lists for the session.
//!
//! Learnt from cursor-agent 2026.10.14:
//! - `session/request_permission` offers `allow-once`, `allow-always` and `reject-once` for a
//!   command outside the person's allowlist (allowlisted ones run without asking).
//! - `cursor/ask_question` sends `{toolCallId, title, questions: [{id, prompt, options: [{id,
//!   label}], allowMultiple}]}` and takes `{outcome: {outcome: "answered", answers:
//!   [{questionId, selectedOptionIds}]}}`.
//! - Models: with `clientCapabilities._meta.parameterizedModelPicker` the session lists plain
//!   model ids (`grok-4.7`), set with `session/set_model`, and each model's settings are
//!   options of their own (`session/set_config_option`: `reasoning_effort` or the like, `fast`,
//!   `thinking`, `context`; names differ by model). Without it, each model is listed once at
//!   the person's saved settings and nothing else is taken. Either way Cursor saves a switch as
//!   its default for next time.
//! - `session/load` streams the chat again as updates; Keleuo has it already (from Cursor's
//!   record), so they are not shown.
//! - ACP sessions are kept in `~/.cursor/acp-sessions/<id>`, the interface's chats in
//!   `~/.cursor/chats/<md5 of the folder>/<id>`, same layout, neither finding the other. A link
//!   from one to the other ([`super::store::share`]) makes them one chat: Terminal opens a
//!   session started in Chat, and ACP continues one started in Terminal or before ACP.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::acp::{self, Connection, Incoming};
use crate::agent::{Agent, Event, Message, Sink, StartOptions};
use crate::host::{Host, ProgramSpec};

/// A request from Cursor waiting for the person.
enum Waiting {
    /// `session/request_permission` (its JSON-RPC id).
    Permission(Value),
    /// `cursor/ask_question` (its id, and its questions as Cursor sent them).
    Question(Value, Vec<Value>),
}

#[derive(Default)]
struct State {
    conn: Option<Arc<Connection>>,
    session: Option<String>,
    /// The session is open (messages before that wait in `queued`).
    ready: bool,
    /// `session/load` is replaying the chat: its updates are not shown.
    loading: bool,
    queued: Vec<(String, Vec<PathBuf>)>,
    waiting: HashMap<String, Waiting>,
    mapper: acp::map::Mapper,
    /// Run everything: permission requests are allowed without asking.
    force: bool,
}

type Shared = Arc<Mutex<State>>;

/// A conversation with Cursor Agent over ACP.
#[derive(Default)]
pub struct CursorAcpAgent {
    state: Shared,
    sink: Option<Sink>,
}

impl Agent for CursorAcpAgent {
    fn running(&self) -> bool {
        self.sink.is_some()
    }

    fn start(&mut self, host: Arc<dyn Host>, options: &StartOptions, sink: Sink) -> anyhow::Result<()> {
        let spec = host.program_command(&ProgramSpec {
            program: "cursor-agent".into(),
            args: vec!["acp".into()],
            cwd: options.cwd.clone(),
            ..Default::default()
        });
        let state = self.state.clone();
        let (s, k) = (state.clone(), sink.clone());
        let conn = Arc::new(Connection::spawn(&spec, move |m| incoming(&s, &k, m))?);
        {
            let mut st = state.lock().unwrap();
            st.conn = Some(conn.clone());
            st.force = matches!(options.permission_mode.as_deref(), Some("force" | "bypassPermissions"));
        }
        let options = options.clone();
        let sink2 = sink.clone();
        // Where Cursor keeps its chats, to share one between Chat and Terminal (this machine).
        let cursor_home = (!host.is_remote()).then(|| host.home_dir().map(|h| h.join(".cursor"))).flatten();
        // The handshake waits on the agent: off the window's thread.
        std::thread::spawn(move || {
            if let Err(e) = open_session(&conn, &state, &options, cursor_home.as_deref(), &sink2) {
                sink2(Message::Event(Event::TurnEnded {
                    error: Some(format!("Cursor could not open the session: {e}")),
                    cost_total_usd: None,
                    input_tokens: 0,
                    output_tokens: 0,
                    duration_ms: None,
                    session_id: None,
                }));
                return;
            }
            let queued = {
                let mut st = state.lock().unwrap();
                st.ready = true;
                std::mem::take(&mut st.queued)
            };
            for (text, images) in queued {
                prompt(&state, &sink2, text, images);
            }
        });
        self.sink = Some(sink);
        Ok(())
    }

    fn send(&mut self, text: &str) -> std::io::Result<()> {
        self.send_with_images(text, &[])
    }

    fn send_with_images(&mut self, text: &str, images: &[PathBuf]) -> std::io::Result<()> {
        let Some(sink) = self.sink.clone() else {
            return Err(std::io::Error::other("Cursor is not started"));
        };
        let ready = {
            let mut st = self.state.lock().unwrap();
            if !st.ready {
                st.queued.push((text.to_string(), images.to_vec()));
            }
            st.ready
        };
        if ready {
            prompt(&self.state, &sink, text.to_string(), images.to_vec());
        }
        Ok(())
    }

    fn takes_images(&self) -> bool {
        true
    }

    fn answer_permission(&mut self, request_id: &str, allow: bool, input: &Value) -> std::io::Result<()> {
        let (conn, waiting) = {
            let mut st = self.state.lock().unwrap();
            (st.conn.clone(), st.waiting.remove(request_id))
        };
        let (Some(conn), Some(waiting)) = (conn, waiting) else { return Ok(()) };
        match waiting {
            Waiting::Permission(id) => conn.respond(&id, acp::map::permission_outcome(input, allow)),
            Waiting::Question(id, questions) => conn.respond(&id, question_answer(&questions, input, allow)),
        }
        Ok(())
    }

    fn interrupt(&mut self) -> std::io::Result<()> {
        let st = self.state.lock().unwrap();
        if let (Some(conn), Some(session)) = (&st.conn, &st.session) {
            conn.notify("session/cancel", json!({"sessionId": session}));
        }
        Ok(())
    }

    fn stop(&mut self) {
        let conn = {
            let mut st = self.state.lock().unwrap();
            let conn = st.conn.take();
            *st = State::default();
            conn
        };
        if let Some(conn) = conn {
            conn.kill();
        }
        self.sink = None;
    }
}

/// Initialize, then continue the chat (`options.resume`) or start one, in the mode and with
/// the model asked for.
fn open_session(conn: &Connection, state: &Shared, options: &StartOptions, cursor_home: Option<&std::path::Path>, sink: &Sink) -> anyhow::Result<()> {
    // One chat for both views: Cursor keeps ACP sessions and its interface's chats apart.
    let share = |id: &str| {
        if let Some(home) = cursor_home
            && let Err(e) = super::store::share(home, &options.cwd, id)
        {
            sink(Message::Event(Event::Notice(format!("Chat and Terminal may show different Cursor chats: {e}."))));
        }
    };
    // Models as plain ids with their settings apart (see the module's notes).
    let mut init = acp::initialize_params();
    init["clientCapabilities"]["_meta"] = json!({"parameterizedModelPicker": true});
    conn.call("initialize", init)?;
    let cwd = options.cwd.to_string_lossy().to_string();
    let mut opened = None;
    if let Some(id) = &options.resume {
        // A chat started in Terminal (or before ACP) is found through the link.
        share(id);
        state.lock().unwrap().loading = true;
        let loaded = conn.call("session/load", json!({"sessionId": id, "cwd": cwd, "mcpServers": []}));
        state.lock().unwrap().loading = false;
        match loaded {
            Ok(r) => opened = Some((id.clone(), r)),
            Err(e) => sink(Message::Event(Event::Notice(format!("Cursor could not continue this chat ({e}); a new one starts.")))),
        }
    }
    let (session, opened) = match opened {
        Some(o) => o,
        None => {
            let r = conn.call("session/new", json!({"cwd": cwd, "mcpServers": []}))?;
            let id = r.get("sessionId").and_then(Value::as_str).ok_or_else(|| anyhow::anyhow!("no session id"))?.to_string();
            share(&id);
            (id, r)
        }
    };
    state.lock().unwrap().session = Some(session.clone());
    // Plan and Ask are Cursor's own modes; the others are its default (Agent), with permission
    // asked or, for Run everything, given here.
    if let Some(mode @ ("plan" | "ask")) = options.permission_mode.as_deref()
        && let Err(e) = conn.call("session/set_mode", json!({"sessionId": session, "modeId": mode}))
    {
        sink(Message::Event(Event::Notice(format!("Cursor did not switch to {mode} mode: {e}"))));
    }
    let mut model = opened.pointer("/models/currentModelId").and_then(Value::as_str).map(str::to_string);
    if let Some(wanted) = &options.model {
        let available = opened.pointer("/models/availableModels").and_then(Value::as_array).cloned().unwrap_or_default();
        match pick_model(&available, wanted) {
            Some(id) => {
                if Some(&id) != model.as_ref() {
                    match conn.call("session/set_model", json!({"sessionId": session, "modelId": id})) {
                        Ok(_) => model = Some(id),
                        Err(e) => sink(Message::Event(Event::Notice(format!("Cursor did not switch to {wanted}: {e}")))),
                    }
                }
                set_variant(conn, &session, wanted, sink);
            }
            None => sink(Message::Event(Event::Notice(format!("Cursor does not offer {wanted} here; it uses {}.", model.clone().unwrap_or_else(|| "its default".into()))))),
        }
    }
    sink(Message::Event(Event::Started {
        session_id: Some(session),
        model,
        permission_mode: options.permission_mode.clone(),
        cwd: Some(cwd),
    }));
    Ok(())
}

/// The model Cursor lists that is `wanted` (`grok-4.7-low-fast` or `grok-4.7`): the one of the
/// same model. Over ACP each model is listed once, at the person's saved settings.
fn pick_model(available: &[Value], wanted: &str) -> Option<String> {
    let base = super::variant(wanted).0;
    let ids = available.iter().filter_map(|m| m.get("modelId").and_then(Value::as_str));
    let mut found = None;
    for id in ids {
        let listed = id.split('[').next().unwrap_or(id);
        if id == wanted || listed == wanted {
            return Some(id.to_string());
        }
        if found.is_none() && (listed == base || (base == "auto" && listed == "default")) {
            found = Some(id.to_string());
        }
    }
    found
}

/// The settings of the variant picked (`grok-4.7-low-fast`: low effort, fast), each set as the
/// model's own option. A model names its effort option its own way: the names it has are
/// tried in turn; one it does not have is skipped. Nothing is set for a plain model id.
fn set_variant(conn: &Connection, session: &str, wanted: &str, sink: &Sink) {
    let (base, effort, thinking, fast) = super::variant(wanted);
    if base == wanted {
        return;
    }
    let set = |id: &str, value: &str| conn.call("session/set_config_option", json!({"sessionId": session, "configId": id, "value": value})).is_ok();
    let mut missed = Vec::new();
    if let Some(effort) = &effort
        && !["reasoning_effort", "effort", "thinking_effort"].iter().any(|id| set(id, effort))
    {
        missed.push(format!("{effort} effort"));
    }
    if !set("fast", if fast { "true" } else { "false" }) && fast {
        missed.push("fast".into());
    }
    if thinking && !set("thinking", "true") {
        missed.push("thinking".into());
    }
    if !missed.is_empty() {
        sink(Message::Event(Event::Notice(format!("Cursor did not take {} for this model.", missed.join(", ")))));
    }
}

/// Run one prompt to its end, on a thread of its own.
fn prompt(state: &Shared, sink: &Sink, text: String, images: Vec<PathBuf>) {
    let (conn, session) = {
        let st = state.lock().unwrap();
        (st.conn.clone(), st.session.clone())
    };
    let (Some(conn), Some(session)) = (conn, session) else { return };
    let (state, sink) = (state.clone(), sink.clone());
    std::thread::spawn(move || {
        let blocks = match acp::prompt_blocks(&text, &images) {
            Ok(b) => b,
            Err(e) => {
                sink(Message::Event(acp::map::turn_ended(&Err(acp::RpcError { code: 0, message: format!("could not read an image: {e}"), data: None }), &session)));
                return;
            }
        };
        let reply = conn.call("session/prompt", json!({"sessionId": session, "prompt": blocks}));
        let mut events = state.lock().unwrap().mapper.finish();
        events.push(acp::map::turn_ended(&reply, &session));
        for e in events {
            sink(Message::Event(e));
        }
    });
}

/// What Cursor sends: updates as events, its requests as prompts for the person (or answered
/// here), the rest as it is.
fn incoming(state: &Shared, sink: &Sink, message: Incoming) {
    match message {
        Incoming::Notification { method, params } if method == "session/update" => {
            let events = {
                let mut st = state.lock().unwrap();
                if st.loading {
                    return;
                }
                st.mapper.update(params.get("update").unwrap_or(&Value::Null))
            };
            for e in events.unwrap_or_default() {
                sink(Message::Event(e));
            }
        }
        Incoming::Notification { .. } => {}
        Incoming::Request { id, method, params } => request(state, sink, id, &method, &params),
        Incoming::Log(line) => sink(Message::Log(line)),
        Incoming::Closed => sink(Message::Closed),
    }
}

fn request(state: &Shared, sink: &Sink, id: Value, method: &str, params: &Value) {
    let request_id = format!("cursor-{}", id.as_str().map_or_else(|| id.to_string(), str::to_string));
    let mut st = state.lock().unwrap();
    let Some(conn) = st.conn.clone() else { return };
    match method {
        "session/request_permission" => {
            let event = acp::map::permission(&request_id, params);
            if st.force {
                let Event::Permission { input, .. } = &event else { return };
                conn.respond(&id, acp::map::permission_outcome(input, true));
                return;
            }
            st.waiting.insert(request_id, Waiting::Permission(id));
            drop(st);
            sink(Message::Event(event));
        }
        "cursor/ask_question" => {
            let questions = params.get("questions").and_then(Value::as_array).cloned().unwrap_or_default();
            st.waiting.insert(request_id.clone(), Waiting::Question(id, questions.clone()));
            drop(st);
            sink(Message::Event(question_card(&request_id, params, &questions)));
        }
        // Anything else of Cursor's (plans, to-dos, ...): not handled yet. Cursor falls back
        // where it can (a question becomes a permission request).
        _ => conn.respond_error(&id, acp::METHOD_NOT_FOUND, "not supported by Keleuo yet"),
    }
}

/// Cursor's questions as Keleuo's question card (the AskUserQuestion form it already draws).
fn question_card(request_id: &str, params: &Value, questions: &[Value]) -> Event {
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let title = s(params, "title");
    let cards: Vec<Value> = questions
        .iter()
        .map(|q| {
            let options: Vec<Value> = q
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|o| json!({"label": s(o, "label"), "description": ""}))
                .collect();
            json!({"question": s(q, "prompt"), "header": title, "options": options,
                   "multiSelect": q.get("allowMultiple").and_then(Value::as_bool).unwrap_or(false)})
        })
        .collect();
    Event::Permission {
        request_id: request_id.to_string(),
        tool: crate::questions::TOOL.into(),
        summary: title,
        description: None,
        input: json!({"questions": cards}),
    }
}

/// The answer Cursor takes for its questions, from the card's (each question's text and the
/// chosen labels, joined with ", "). Refused: cancelled.
fn question_answer(questions: &[Value], input: &Value, allow: bool) -> Value {
    if !allow {
        return json!({"outcome": {"outcome": "cancelled"}});
    }
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let answers: Vec<Value> = questions
        .iter()
        .map(|q| {
            let chosen = input.pointer("/answers").and_then(|a| a.get(s(q, "prompt"))).and_then(Value::as_str).unwrap_or_default();
            let picked: Vec<&str> = chosen.split(", ").collect();
            let ids: Vec<Value> = q
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|o| picked.contains(&s(o, "label").as_str()))
                .map(|o| json!(s(o, "id")))
                .collect();
            json!({"questionId": s(q, "id"), "selectedOptionIds": ids})
        })
        .collect();
    json!({"outcome": {"outcome": "answered", "answers": answers}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_is_a_card_and_its_answer_cursors() {
        let params = json!({"toolCallId": "t", "title": "Colour", "questions": [
            {"id": "q1", "prompt": "Which colour do you prefer?", "allowMultiple": false,
             "options": [{"id": "r", "label": "Red"}, {"id": "g", "label": "Green"}]},
            {"id": "q2", "prompt": "Which sizes?", "allowMultiple": true,
             "options": [{"id": "s", "label": "Small"}, {"id": "l", "label": "Large"}]}]});
        let questions = params["questions"].as_array().unwrap().clone();
        let Event::Permission { tool, input, .. } = question_card("cursor-1", &params, &questions) else { panic!() };
        let parsed = crate::questions::parse(&tool, &input).unwrap();
        assert_eq!(parsed[0].question, "Which colour do you prefer?");
        assert!(parsed[1].multi);
        let answered = crate::questions::answered(&input, &[
            ("Which colour do you prefer?".into(), vec!["Green".into()]),
            ("Which sizes?".into(), vec!["Small".into(), "Large".into()]),
        ]);
        assert_eq!(
            question_answer(&questions, &answered, true),
            json!({"outcome": {"outcome": "answered", "answers": [
                {"questionId": "q1", "selectedOptionIds": ["g"]},
                {"questionId": "q2", "selectedOptionIds": ["s", "l"]}]}})
        );
        assert_eq!(question_answer(&questions, &answered, false), json!({"outcome": {"outcome": "cancelled"}}));
    }

    #[test]
    fn a_picked_model_is_the_one_cursor_lists() {
        // The parameterized picker: plain ids.
        let available = vec![json!({"modelId": "default", "name": "Auto"}), json!({"modelId": "grok-4.7", "name": "Grok 4.7"}), json!({"modelId": "composer-2.5", "name": "Composer 2.5"})];
        assert_eq!(pick_model(&available, "grok-4.7-low-fast").as_deref(), Some("grok-4.7"));
        assert_eq!(pick_model(&available, "composer-2.5").as_deref(), Some("composer-2.5"));
        assert_eq!(pick_model(&available, "auto").as_deref(), Some("default"));
        assert_eq!(pick_model(&available, "gpt-9"), None);
        // The variants picker: one id per model, its settings in brackets.
        let variants = vec![json!({"modelId": "grok-4.7[context=256k,reasoning_effort=high,fast=true]", "name": "grok-4.7"})];
        assert_eq!(pick_model(&variants, "grok-4.7-low").as_deref(), Some("grok-4.7[context=256k,reasoning_effort=high,fast=true]"));
    }

    /// The agent against the real `cursor-agent acp` (signed in; no model switch, so the
    /// person's Cursor settings stay as they are): a reply, a permission asked and allowed,
    /// and a turn stopped part way.
    /// `cargo test -p keleuo-core acp_agent_for_real -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn acp_agent_for_real() {
        use std::sync::mpsc::channel;
        use std::time::Duration;
        let dir = std::env::temp_dir().join(format!("keleuo-cacp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let (tx, rx) = channel::<Message>();
        let tx = Mutex::new(tx);
        let mut agent = CursorAcpAgent::default();
        let options = StartOptions { cwd: dir.clone(), ..Default::default() };
        agent.start(Arc::new(crate::host::LocalHost), &options, Arc::new(move |m| drop(tx.lock().unwrap().send(m)))).unwrap();
        // Until the turn ends; a permission asked is answered with `allow`.
        let turn = |agent: &mut CursorAcpAgent, allow: Option<bool>| {
            let mut got = Vec::new();
            loop {
                match rx.recv_timeout(Duration::from_secs(120)).expect("the turn ends") {
                    Message::Event(e @ (Event::TurnEnded { .. } | Event::Interrupted)) => {
                        got.push(e);
                        return got;
                    }
                    Message::Event(Event::Permission { request_id, input, summary, .. }) => {
                        eprintln!("asked: {summary}");
                        agent.answer_permission(&request_id, allow.unwrap_or(false), &input).unwrap();
                        got.push(Event::Notice(format!("asked: {summary}")));
                    }
                    Message::Event(e) => got.push(e),
                    _ => {}
                }
            }
        };
        let text = |events: &[Event]| -> String {
            events.iter().filter_map(|e| if let Event::TextDelta(t) = e { Some(t.as_str()) } else { None }).collect()
        };
        agent.send("Reply with just the word: pong").unwrap();
        let first = turn(&mut agent, None);
        eprintln!("first: {:?}", text(&first));
        assert!(text(&first).to_lowercase().contains("pong"));
        assert!(first.iter().any(|e| matches!(e, Event::Started { session_id: Some(_), .. })));
        agent.send("Run exactly this shell command: touch made_by_test.txt").unwrap();
        let second = turn(&mut agent, Some(true));
        assert!(second.iter().any(|e| matches!(e, Event::Notice(n) if n.starts_with("asked:"))), "a permission was asked");
        assert!(dir.join("made_by_test.txt").exists(), "allowed, it ran");
        agent.send("Write a 1500 word story about a heron, plain prose, no tools.").unwrap();
        std::thread::sleep(Duration::from_secs(6));
        agent.interrupt().unwrap();
        let third = turn(&mut agent, None);
        eprintln!("third ended: {:?}", third.last());
        assert_eq!(third.last(), Some(&Event::Interrupted));
        agent.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
