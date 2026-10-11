//! Replays of real `claude` stream-json output, recorded against a throwaway folder with a cheap
//! model and cleaned of local paths (see `fixtures/`).

use workbench_core::conversation::{Conversation, Item};
use workbench_core::diff::LineKind;
use workbench_core::stream::{Event, parse_line};

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
    chat.push_user("change the greeting");
    let applied = chat.apply_lines(&text);
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
    // Up to the permission request: the turn waits.
    let upto: Vec<&str> = text
        .lines()
        .take_while(|l| !l.contains("\"control_request\""))
        .collect();
    let request = text
        .lines()
        .find(|l| l.contains("\"control_request\""))
        .unwrap();
    chat.apply_lines(&upto.join("\n"));
    chat.apply_lines(request);
    let pending: Vec<_> = chat.pending_permissions().cloned().collect();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].tool_name, "Bash");
    assert_eq!(pending[0].summary, "echo workbench-test > note.txt");
    assert_eq!(chat.status.as_deref(), Some("waiting for permission"));

    let input = chat
        .answer_permission(&pending[0].request_id, true)
        .unwrap();
    let reply: serde_json::Value = serde_json::from_str(
        &workbench_core::stream::permission_response(&pending[0].request_id, true, &input),
    )
    .unwrap();
    assert_eq!(reply["type"], "control_response");
    assert_eq!(reply["response"]["response"]["behavior"], "allow");
    assert_eq!(
        reply["response"]["response"]["updatedInput"]["command"],
        "echo workbench-test > note.txt"
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
    chat.apply_lines(&rest.join("\n"));
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
    let applied = chat.apply_lines(&fixture("write_turn.jsonl"));
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
    use workbench_core::conversation::AgentStatus;
    let text = fixture("subagents_turn.jsonl");
    let mut chat = Conversation::new("");
    chat.push_user("launch two subagents");
    // Replay up to the first progress report: both running, one using Read.
    let lines: Vec<&str> = text.lines().collect();
    let first_progress = lines
        .iter()
        .position(|l| l.contains("\"task_progress\""))
        .unwrap();
    chat.apply_lines(&lines[..=first_progress].join("\n"));
    assert_eq!(chat.agents.len(), 2);
    assert!(chat.agents.iter().all(|a| a.running()));
    assert_eq!(chat.agents[0].description, "Read greet.py function name");
    assert_eq!(chat.agents[0].kind, "general-purpose");
    assert_eq!(chat.agents[0].current_tool.as_deref(), Some("Read"));
    assert_eq!(chat.running_agents().count(), 2);
    // The rest of the turn: both done, with their own tool calls and token counts.
    chat.apply_lines(&lines[first_progress + 1..].join("\n"));
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
