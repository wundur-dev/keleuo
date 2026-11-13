//! Starting fresh from a summary: a session grown too big to carry on (an agent that crashes
//! loading it, a context that fills at once) is handed over to a new one. The handoff is drafted
//! from what Wundur already holds, so it works when the old session can't be asked: the
//! agent's own last compaction summary when there is one, the turns since (the person's
//! messages, the agent's last words each turn), and the files they changed. With a local model
//! set up (Settings > Summaries), it is condensed into a brief.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use serde_json::{Value, json};

use crate::conversation::{Conversation, Item};

/// The most the draft holds; the earliest turns go first when there is more.
const MAX_DRAFT: usize = 24_000;
/// The most of one message or reply kept in the draft.
const MAX_ONE: usize = 1_500;

fn cut(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}…")
}

/// The handoff drafted from `chat` (no model), the session called `name`: what the next
/// session needs to carry on.
pub fn draft(chat: &Conversation, folder: &Path, name: &str) -> String {
    let items = &chat.items;
    let compacted = items.iter().rposition(|i| matches!(i, Item::Compacted { .. }));
    let from = compacted.map_or(0, |i| i + 1);

    // The turns since: each the person's message and the agent's last words in it.
    let mut turns: Vec<String> = Vec::new();
    let mut ask: Option<String> = None;
    let mut reply = String::new();
    let flush = |ask: &mut Option<String>, reply: &mut String, turns: &mut Vec<String>| {
        if let Some(a) = ask.take() {
            let mut t = format!("**Asked:** {}", cut(&crate::paste::without_markers(&a), MAX_ONE));
            if !reply.trim().is_empty() {
                t.push_str(&format!("\n**Answer:** {}", cut(reply, MAX_ONE)));
            }
            turns.push(t);
        }
        reply.clear();
    };
    for item in &items[from..] {
        match item {
            Item::User(text) => {
                flush(&mut ask, &mut reply, &mut turns);
                ask = Some(text.clone());
            }
            // The last thing said in the turn is what it ended on.
            Item::Assistant { text, .. } if !text.trim().is_empty() => reply = text.clone(),
            _ => {}
        }
    }
    flush(&mut ask, &mut reply, &mut turns);

    let mut out = format!("# Carrying on from an earlier session\n\nFolder: `{}`", folder.display());
    if !name.trim().is_empty() {
        out.push_str(&format!(" · session \"{}\"", name.trim()));
    }
    out.push_str("\n\nThe earlier session grew too big to continue, so this one starts from a summary of it.\n");
    if let Some(Item::Compacted { summary, .. }) = compacted.map(|i| &items[i]) {
        out.push_str(&format!("\n## Summary of the work so far\n\n{}\n", summary.trim()));
    }
    let changed = chat.changes(from);
    if !changed.is_empty() {
        let names: Vec<String> = changed.iter().take(60).map(|c| format!("`{}`", c.path.strip_prefix(folder).unwrap_or(&c.path).display())).collect();
        out.push_str(&format!("\n## Files changed since\n\n{}\n", names.join(", ")));
    }
    // The latest turns, as many as fit.
    let mut kept: Vec<String> = Vec::new();
    let mut size = out.len();
    for t in turns.iter().rev() {
        if size + t.len() > MAX_DRAFT {
            break;
        }
        size += t.len() + 2;
        kept.push(t.clone());
    }
    kept.reverse();
    if !kept.is_empty() {
        let skipped = turns.len() - kept.len();
        let head = if skipped > 0 { format!("the last {} of {}", kept.len(), turns.len()) } else { "all of them".into() };
        out.push_str(&format!("\n## Recent turns ({head})\n\n{}\n", kept.join("\n\n")));
    }
    out.push_str("\nRead this, then say briefly where things stand and wait for what I want next.");
    out
}

/// What the local model is told.
const CONDENSE: &str = "You write a handoff for a coding agent starting a fresh session. Below is what an \
earlier session left: a summary of its work, the files it changed, and its recent turns. Write a brief \
the new session can work from, in Markdown, with these sections: Goal (what the person is building), \
Where things stand (what works, what was just done), Open threads (unfinished work, known bugs, decisions \
pending), Key files (with what each is for), and Notes (conventions, preferences, things not to redo). \
Keep facts, names, paths and numbers exact; leave out chatter. At most about 600 words. End with the \
line: Read this, then say briefly where things stand and wait for what I want next.";

/// The draft condensed by the local model the person set up for summaries (blocking: off the
/// UI thread). The folder's line is kept above it.
pub fn condense(config: &crate::summary::Config, draft: &str) -> Result<String> {
    let base = {
        let e = config.endpoint.trim().trim_end_matches('/');
        if e.ends_with("/v1") { e.to_string() } else { format!("{e}/v1") }
    };
    let http: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(300))).build().into();
    let mut req = http.post(&format!("{base}/chat/completions"));
    if !config.api_key.trim().is_empty() {
        req = req.header("Authorization", format!("Bearer {}", config.api_key.trim()));
    }
    let body = json!({
        "model": config.model.trim(),
        "temperature": 0.2,
        "max_tokens": 2000,
        "messages": [
            { "role": "system", "content": CONDENSE },
            { "role": "user", "content": draft },
        ],
    });
    let response: Value = req.send_json(body).with_context(|| format!("could not reach {base}"))?.body_mut().read_json()?;
    let text = response["choices"][0]["message"]["content"].as_str().ok_or_else(|| anyhow!("no answer"))?.trim();
    if text.is_empty() {
        return Err(anyhow!("an empty answer"));
    }
    let first = draft.lines().take(3).collect::<Vec<_>>().join("\n");
    Ok(format!("{first}\n\n{text}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(text: &str) -> Item {
        Item::Assistant { text: text.into(), streaming: false }
    }

    #[test]
    fn the_draft_starts_from_the_last_compaction_and_keeps_each_turns_last_words() {
        let mut chat = Conversation::default();
        chat.items = vec![
            Item::User("old work".into()),
            said("long gone"),
            Item::Compacted { summary: "Built the dragon level; fire works.".into(), trigger: Some("auto".into()), tokens: Some(150_000) },
            Item::User("add a boss".into()),
            said("Looking at the level…"),
            said("Added the boss with 3 phases."),
            Item::User("the boss falls through the floor".into()),
            said("Fixed: its collider was a trigger."),
        ];
        let text = draft(&chat, Path::new("/games/dragon"), "Dragon game");
        assert!(text.contains("Folder: `/games/dragon` · session \"Dragon game\""), "{text}");
        assert!(text.contains("## Summary of the work so far\n\nBuilt the dragon level; fire works."), "{text}");
        assert!(!text.contains("old work") && !text.contains("long gone"), "before the compaction: in its summary");
        assert!(text.contains("**Asked:** add a boss\n**Answer:** Added the boss with 3 phases."), "{text}");
        assert!(!text.contains("Looking at the level"), "only the turn's last words");
        assert!(text.contains("**Asked:** the boss falls through the floor\n**Answer:** Fixed: its collider was a trigger."), "{text}");
        assert!(text.trim_end().ends_with("wait for what I want next."));
    }

    /// The real local model condensing a draft: `WUNDUR_SUMMARY_ENDPOINT=http://host:1234
    /// WUNDUR_SUMMARY_MODEL=<id> cargo test -p wundur-core handoff::tests::condensed -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn condensed() {
        let config = crate::summary::Config {
            endpoint: std::env::var("WUNDUR_SUMMARY_ENDPOINT").unwrap(),
            model: std::env::var("WUNDUR_SUMMARY_MODEL").unwrap(),
            ..Default::default()
        };
        let mut chat = Conversation::default();
        chat.items = vec![
            Item::Compacted { summary: "Unity 2D platformer 'Dragon Run'. Player (Assets/Scripts/Player.cs) moves and breathes fire on Space; level 1 built with tilemaps; coins counted in GameManager.cs. Person prefers small commits and pixel-art style.".into(), trigger: None, tokens: None },
            Item::User("add a boss to the dragon level".into()),
            said("Added Boss.cs: 3 phases (walk, charge, fireball), 30 HP, spawns at the arena trigger."),
            Item::User("the boss falls through the floor".into()),
            said("Fixed: the boss's BoxCollider2D was set as a trigger. Unchecked it; it now stands on the tilemap."),
            Item::User("make the music change when the boss appears".into()),
            said("Started: added MusicManager.cs with a crossfade, but the boss track isn't imported yet: you need to add boss_theme.mp3 to Assets/Audio."),
        ];
        let text = condense(&config, &draft(&chat, Path::new("/games/dragon-run"), "Dragon Run")).unwrap();
        println!("{text}");
        assert!(text.contains("Boss") && text.contains("wait for what I want next"));
    }

    #[test]
    fn a_huge_session_keeps_its_latest_turns() {
        let mut chat = Conversation::default();
        for n in 0..400 {
            chat.items.push(Item::User(format!("turn {n} {}", "x".repeat(300))));
            chat.items.push(said(&format!("done {n} {}", "y".repeat(300))));
        }
        let text = draft(&chat, Path::new("/g"), "");
        assert!(text.len() < MAX_DRAFT + 2_000, "{}", text.len());
        assert!(text.contains("turn 399") && !text.contains("turn 0 "), "the latest kept, the earliest dropped");
        assert!(text.contains("of 400)"), "says how many were left out");
    }
}
