//! Replays of real `claude` stream-json output, recorded against a throwaway folder with a cheap
//! model and cleaned of local paths (see `fixtures/`).

use keleuo_core::conversation::{Conversation, Item};
use keleuo_core::diff::LineKind;
use keleuo_core::backends::claude::map::Mapper;
use keleuo_core::backends::claude::stream::{Event, parse_line};
use keleuo_core::conversation::Applied;

/// Recorded Claude Code lines, mapped onto neutral events and applied.
fn apply_lines(chat: &mut Conversation, m: &mut Mapper, text: &str) -> Vec<Applied> {
    m.map_lines(text).into_iter().map(|e| chat.apply(e)).collect()
}

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn every_fixture_line_parses_as_json() {
    for name in [
        "edit_turn.jsonl",
        "permission_bash_turn.jsonl",
        "write_turn.jsonl",
    ] {
        for line in fixture(name).lines() {
            let event = parse_line(line).unwrap();
            assert!(!matches!(event, Event::Unparsed(_)), "{name}: {line}");
        }
    }
}

#[test]
fn edit_turn_streams_text_and_builds_a_diff() {
    let text = fixture("edit_turn.jsonl");
    let events: Vec<Event> = text.lines().filter_map(parse_line).collect();
    match &events[0] {
        Event::Init {
            model,
            permission_mode,
            cwd,
            ..
        } => {
            assert_eq!(model, "claude-haiku-5-5");
            assert_eq!(permission_mode, "default");
            assert_eq!(cwd, "/tmp/wb-proj");
        }
        e => panic!("first event {e:?}"),
    }
    let deltas: String = events
        .iter()
        .filter_map(|e| match e {
            Event::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        deltas.contains("hi"),
        "streamed text arrives in deltas: {deltas}"
    );

    let mut chat = Conversation::new("");
    let mut m = Mapper::new("");
    chat.push_user("change the greeting");
    let applied = apply_lines(&mut chat, &mut m, &text);
    assert!(!chat.busy);
    assert!(applied.iter().any(|a| a.turn_ended));
    let changed: Vec<_> = applied
        .iter()
        .filter_map(|a| a.file_changed.clone())
        .collect();
    assert_eq!(changed, [std::path::PathBuf::from("/tmp/wb-proj/greet.py")]);

    let tools: Vec<_> = chat
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Tool(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].name, "Read");
    assert_eq!(
        tools[0].summary, "greet.py",
        "paths relative to the session cwd"
    );
    assert!(tools[0].result.as_deref().unwrap().contains("def greet"));
    assert!(tools[0].diff.is_none());
    let edit = tools[1];
    assert_eq!(edit.name, "Edit");
    let diff = edit.diff.as_ref().unwrap();
    assert_eq!(diff.hunks[0].old_start, 1);
    let changed: Vec<_> = diff.hunks[0]
        .lines
        .iter()
        .filter(|l| l.kind != LineKind::Context)
        .map(|l| (l.kind, l.text.trim()))
        .collect();
    assert_eq!(
        changed,
        [
            (LineKind::Removed, "return \"hello \" + name"),
            (LineKind::Added, "return \"hi \" + name")
        ]
    );

    let texts: Vec<_> = chat
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Assistant { text, streaming } => Some((text.as_str(), *streaming)),
            _ => None,
        })
        .collect();
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(texts[0].0.starts_with("Changed the greeting"));
    assert!(!texts[0].1, "final text replaces the streamed one");

    let cost = chat.last_turn_cost_usd.unwrap();
    assert!(cost > 0.0 && cost < 0.05, "{cost}");
}

#[test]
fn permission_prompt_is_reported_and_answered() {
    let text = fixture("permission_bash_turn.jsonl");
    let mut chat = Conversation::new("");
    let mut m = Mapper::new("");
    // Up to the permission request: the turn waits.
    let upto: Vec<&str> = text
        .lines()
        .take_while(|l| !l.contains("\"control_request\""))
        .collect();
    let request = text
        .lines()
        .find(|l| l.contains("\"control_request\""))
        .unwrap();
    apply_lines(&mut chat, &mut m, &upto.join("\n"));
    apply_lines(&mut chat, &mut m, request);
    let pending: Vec<_> = chat.pending_permissions().cloned().collect();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].tool_name, "Bash");
    assert_eq!(pending[0].summary, "echo keleuo-test > note.txt");
    assert_eq!(chat.status.as_deref(), Some("waiting for permission"));

    let input = chat
        .answer_permission(&pending[0].request_id, true)
        .unwrap();
    let reply: serde_json::Value = serde_json::from_str(
        &keleuo_core::backends::claude::stream::permission_response(&pending[0].request_id, true, &input),
    )
    .unwrap();
    assert_eq!(reply["type"], "control_response");
    assert_eq!(reply["response"]["response"]["behavior"], "allow");
    assert_eq!(
        reply["response"]["response"]["updatedInput"]["command"],
        "echo keleuo-test > note.txt"
    );
    assert!(!chat.needs_input());
    assert_eq!(chat.title(10).as_deref(), None);
    assert_eq!(chat.pending_permissions().count(), 0);
    assert!(
        chat.answer_permission(&pending[0].request_id, true)
            .is_none(),
        "answered once only"
    );

    let rest: Vec<&str> = text
        .lines()
        .skip_while(|l| !l.contains("\"control_request\""))
        .skip(1)
        .collect();
    apply_lines(&mut chat, &mut m, &rest.join("\n"));
    let bash = chat
        .items
        .iter()
        .find_map(|i| match i {
            Item::Tool(t) if t.name == "Bash" => Some(t),
            _ => None,
        })
        .unwrap();
    assert!(bash.done());
    assert!(!bash.is_error);
}

#[test]
fn write_turn_gives_a_created_file_diff() {
    let mut chat = Conversation::new("");
    let mut m = Mapper::new("");
    let applied = apply_lines(&mut chat, &mut m, &fixture("write_turn.jsonl"));
    let write = chat
        .items
        .iter()
        .find_map(|i| match i {
            Item::Tool(t) if t.name == "Write" => Some(t),
            _ => None,
        })
        .unwrap();
    let diff = write.diff.as_ref().unwrap();
    assert!(diff.created);
    assert_eq!(diff.hunks[0].lines[0].text, "# Notes");
    assert!(
        diff.hunks[0]
            .lines
            .iter()
            .all(|l| l.kind == LineKind::Added)
    );
    assert!(
        applied
            .iter()
            .any(|a| a.file_changed.as_deref()
                == Some(std::path::Path::new("/tmp/wb-proj/notes.md")))
    );
    let last_text = chat
        .items
        .iter()
        .rev()
        .find_map(|i| match i {
            Item::Assistant { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        last_text.contains("**"),
        "markdown kept for rendering: {last_text}"
    );
}

#[test]
fn subagents_are_tracked_live() {
    use keleuo_core::conversation::AgentStatus;
    let text = fixture("subagents_turn.jsonl");
    let mut chat = Conversation::new("");
    let mut m = Mapper::new("");
    chat.push_user("launch two subagents");
    // Replay up to the first progress report: both running, one using Read.
    let lines: Vec<&str> = text.lines().collect();
    let first_progress = lines
        .iter()
        .position(|l| l.contains("\"task_progress\""))
        .unwrap();
    apply_lines(&mut chat, &mut m, &lines[..=first_progress].join("\n"));
    assert_eq!(chat.agents.len(), 2);
    assert!(chat.agents.iter().all(|a| a.running()));
    assert_eq!(chat.agents[0].description, "Read greet.py function name");
    assert_eq!(chat.agents[0].kind, "general-purpose");
    assert_eq!(chat.agents[0].current_tool.as_deref(), Some("Read"));
    assert_eq!(chat.running_agents().count(), 2);
    // The rest of the turn: both done, with their own tool calls and token counts.
    apply_lines(&mut chat, &mut m, &lines[first_progress + 1..].join("\n"));
    assert!(chat.agents.iter().all(|a| a.status == AgentStatus::Done));
    assert_eq!(chat.running_agents().count(), 0);
    assert_eq!(chat.turn_agents().len(), 2);
    let reader = &chat.agents[0];
    assert!(reader.total_tokens.unwrap() > 1000);
    assert!(reader.duration_ms.unwrap() > 0);
    assert!(reader.summary.as_deref().unwrap().contains("greet"));
    let tools: Vec<&str> = reader
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Tool(t) => Some(t.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(tools, ["Read"]);
    assert!(matches!(&reader.items[0], Item::Tool(t) if t.done()));
    // The subagents' calls stay out of the main list; the Agent calls are there.
    let main_tools: Vec<&str> = chat
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Tool(t) => Some(t.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(main_tools, ["Agent", "Agent"]);
}

/// A command run in the background is a task to Claude Code (`task_type: local_bash`), with
/// the same task events as a subagent; it stays a tool call, not an agent. Lines from claude
/// 2.1.299, cut to the fields that matter.
#[test]
fn a_background_command_is_not_a_subagent() {
    let lines = [
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_bg","name":"Bash","input":{"command":"sleep 70","description":"Sleep for 70 seconds in background","run_in_background":true}}]},"parent_tool_use_id":null}"#,
        r#"{"type":"system","subtype":"task_started","task_id":"bs4n7wtbi","tool_use_id":"toolu_bg","description":"Sleep for 70 seconds in background","is_backgrounded":true,"task_type":"local_bash"}"#,
        r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_bg","type":"tool_result","content":"Command running in background with ID: bs4n7wtbi.","is_error":false}]},"parent_tool_use_id":null}"#,
        r#"{"type":"system","subtype":"task_updated","task_id":"bs4n7wtbi","patch":{"status":"completed"}}"#,
        r#"{"type":"system","subtype":"task_notification","task_id":"bs4n7wtbi","tool_use_id":"toolu_bg","status":"completed","summary":"Background command completed (exit code 0)"}"#,
    ];
    let mut chat = Conversation::new("");
    let mut m = Mapper::new("");
    chat.push_user("run it in the background");
    apply_lines(&mut chat, &mut m, &lines.join("\n"));
    assert!(chat.agents.is_empty());
    assert!(chat.items.iter().any(|i| matches!(i, Item::Tool(t) if t.name == "Bash")));
}

/// A Cursor Agent turn: its reply streamed then given whole, a read, a new file with its diff,
/// a command with its output, and the turn's end with the chat id and token counts.
#[test]
fn cursor_turn_maps_onto_a_chat() {
    use keleuo_core::backends::cursor::map::Mapper as CursorMapper;
    let mut m = CursorMapper::new("/tmp/p", Some("default".into()));
    let mut chat = Conversation::new("");
    chat.push_user("read, write, ls");
    let events: Vec<_> = fixture("cursor_turn.jsonl").lines().flat_map(|l| m.map_line(l)).collect();
    for e in events.clone() {
        chat.apply(e);
    }
    assert!(m.ended);
    assert_eq!(chat.session_id.as_deref(), Some("a9d634e3-ade4-48c2-bc23-2cd4de56f6c4"));
    let tools: Vec<(&str, &str)> = chat
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Tool(t) => Some((t.name.as_str(), t.summary.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(tools, [("Read", "hello.txt"), ("Edit", "bye.txt"), ("Shell", "ls")]);
    let created = events.iter().find_map(|e| match e {
        keleuo_core::agent::Event::ToolResult { diff: Some(d), file_changed, .. } => Some((d.clone(), file_changed.clone())),
        _ => None,
    });
    let (diff, changed) = created.unwrap();
    assert!(diff.created);
    assert_eq!(diff.added(), 1);
    assert_eq!(changed.unwrap(), std::path::PathBuf::from("/tmp/p/bye.txt"));
    // The reply: each block whole, as written.
    let replies: Vec<&str> = chat
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Assistant { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(replies.len(), 2);
    assert!(replies[1].contains("Created `bye.txt`"));
    assert!(events.iter().any(|e| matches!(e, keleuo_core::agent::Event::TurnEnded { error: None, input_tokens, .. } if *input_tokens > 10_000)));
}

/// Cursor's Task tool starts subagents: each a subagent card with its description, ended with
/// what it said; the reply's whole blocks replace their pieces (no text twice).
#[test]
fn cursor_tasks_are_subagents() {
    use keleuo_core::backends::cursor::map::Mapper as CursorMapper;
    use keleuo_core::conversation::AgentStatus;
    let mut m = CursorMapper::new("/tmp/p", None);
    let mut chat = Conversation::new("");
    chat.push_user("two subagents");
    for e in fixture("cursor_subagents.jsonl").lines().flat_map(|l| m.map_line(l)) {
        chat.apply(e);
    }
    assert_eq!(chat.agents.len(), 2);
    let described: Vec<&str> = chat.agents.iter().map(|a| a.description.as_str()).collect();
    assert_eq!(described, ["Read hello.txt content", "Read bye.txt content"]);
    assert!(chat.agents.iter().all(|a| a.status == AgentStatus::Done));
    assert_eq!(chat.agents[1].summary.as_deref(), Some("bye"));
    assert!(chat.agents[0].duration_ms.unwrap() > 0);
    let replies: Vec<&str> = chat
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Assistant { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        replies,
        [
            "Using the parallel-agents skill, then launching two Task subagents to read the files.",
            "Using dispatching-parallel-agents — launching both file-reader subagents now.",
            "**hello.txt** is `hello`; **bye.txt** is `bye`.",
        ]
    );
}

/// How full the context is comes from the turn's last request, not from the `result`'s usage,
/// which adds up every request the turn made (a turn of many tool calls counted the context
/// many times over: "259% of context").
#[test]
fn the_context_is_the_last_requests_not_the_turns_total() {
    let lines = [
        r#"{"type":"assistant","message":{"id":"m1","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/tmp/a"}}],"usage":{"input_tokens":2,"cache_read_input_tokens":9000,"cache_creation_input_tokens":998,"output_tokens":50}},"parent_tool_use_id":null}"#,
        r#"{"type":"assistant","message":{"id":"m2","model":"claude-opus-5-5","content":[{"type":"text","text":"done"}],"usage":{"input_tokens":2,"cache_read_input_tokens":11000,"cache_creation_input_tokens":998,"output_tokens":20}},"parent_tool_use_id":null}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":2,"result":"done","session_id":"s1","total_cost_usd":0.1,"usage":{"input_tokens":4,"cache_read_input_tokens":48000,"cache_creation_input_tokens":1996,"output_tokens":70}}"#,
    ];
    let mut m = Mapper::new("");
    let ended = m.map_lines(&lines.join("\n")).into_iter().find_map(|e| match e {
        keleuo_core::agent::Event::TurnEnded { input_tokens, .. } => Some(input_tokens),
        _ => None,
    });
    assert_eq!(ended, Some(12_000));
}
