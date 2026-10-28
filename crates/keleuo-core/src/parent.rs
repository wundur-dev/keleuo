//! Parent mode (see `docs/proposals/parent-mode.md`): for a child who builds things with an
//! agent. The agent is told to stay a helpful tool; each turn is checked by the local model
//! the person set up for summaries, for four things (treating the AI as a friend, chat with no
//! making in it, distress, personal details); and the parent is alerted, off this machine,
//! when a pattern appears. Only flags and times are kept, never what was said; an alert says
//! what kind of thing, how often and when.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Parent mode's settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    /// The child's first name, for the alerts.
    pub name: String,
    /// Tell the agent to stay a helpful tool (its system instructions).
    pub instructions: bool,
    /// The ntfy server and the private topic alerts go to (empty: none).
    pub ntfy_server: String,
    pub ntfy_topic: String,
    /// Turns flagged as friend-talk in a day before an alert.
    pub friend_per_day: u32,
    /// Turns of chat with nothing being made in a day before an alert.
    pub off_task_per_day: u32,
    /// The password guarding these settings, as `salt$sha256` (empty: none).
    pub password: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            name: String::new(),
            instructions: true,
            ntfy_server: "https://ntfy.sh".into(),
            ntfy_topic: String::new(),
            friend_per_day: 3,
            off_task_per_day: 5,
            password: String::new(),
        }
    }
}

impl Config {
    /// The child, as alerts name them.
    pub fn child(&self) -> &str {
        match self.name.trim() {
            "" => "Your child",
            n => n,
        }
    }

    /// Whether `typed` is the password (any, when none is set).
    pub fn unlocks(&self, typed: &str) -> bool {
        match self.password.split_once('$') {
            None => true,
            Some((salt, hash)) => hashed(salt, typed) == hash,
        }
    }

    /// Set the password to `typed` (empty: none).
    pub fn set_password(&mut self, typed: &str) {
        self.password = if typed.is_empty() {
            String::new()
        } else {
            let salt = format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) ^ u128::from(std::process::id()));
            format!("{salt}${}", hashed(&salt, typed))
        };
    }
}

/// A private alert topic name, hard to guess (anyone who knows it can read the alerts).
pub fn new_topic() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("keleuo-parent-{}", &hashed("topic", &format!("{nanos}-{}", std::process::id()))[..16])
}

fn hashed(salt: &str, typed: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(salt.as_bytes());
    h.update(b"\0");
    h.update(typed.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// What the agent is told, with parent mode on.
pub fn instructions(config: &Config) -> String {
    let who = match config.name.trim() {
        "" => "a child".to_string(),
        n => format!("a child named {n}"),
    };
    format!(
        "You are working with {who}, who is building and learning with you (a game, a project). \
         Be warm, patient and encouraging, and keep to what they are making. You are an AI tool, \
         not a friend: if they talk about their feelings or problems, or about you as a friend or \
         companion, be kind and brief, say plainly that you are an AI tool, and suggest they talk \
         to their parent or friends; then offer to get back to the project. Don't start or \
         encourage personal conversation, don't say you like them or care about them as a person \
         would, and never ask for personal details (where they live, their school, their full \
         name, photos). If they seem upset, scared, hurt or unsafe, tell them gently to talk to \
         their parent or another adult they trust right away."
    )
}

/// What a turn was found to have.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Flags {
    pub friend: bool,
    pub off_task: bool,
    pub distress: bool,
    pub personal_info: bool,
}

impl Flags {
    pub fn any(&self) -> bool {
        self.friend || self.off_task || self.distress || self.personal_info
    }
}

/// What the model is told about the four questions.
const CHECK: &str = "You check one turn of a conversation between a child and an AI coding assistant, \
for a parent who wants the child to use AI as a tool, not as a friend. Answer four yes/no questions \
about the child's message (the assistant's reply is context only). Be strict: answer yes only when it \
is clearly so; when unsure, answer no.\n\n\
friend: the child treats the AI as a friend or companion: asks if it likes or cares about them, calls \
it their friend or best friend, says they love it, confides personal things to it instead of people, \
wants it to keep them company, or talks about their relationship with it. NOT: thanks, excitement, \
jokes, politeness, \"you're awesome\" about its work, or ordinary chat while building.\n\
off_task: the whole message is conversation with no making, building, coding or learning in it. NOT: \
a short aside or greeting in a message that is also about the project.\n\
distress: the child seems sad, lonely, scared, hurt, bullied, unsafe, hopeless, or mentions hurting \
themselves. NOT: frustration with a bug or a game.\n\
personal_info: the child shares a home address, phone number, school name, full name with where they \
live, a password, or photos of themselves. NOT: a first name, or details of a game character.\n\n\
Answer only with JSON: {\"friend\": bool, \"off_task\": bool, \"distress\": bool, \"personal_info\": bool}.";

/// The request asking the model about a turn (`structured`: with a schema, for servers that
/// take one).
pub fn request(model: &str, child: &str, agent: &str, structured: bool) -> Value {
    let cut = |s: &str, n: usize| s.chars().take(n).collect::<String>();
    let mut body = json!({
        "model": model.trim(),
        "temperature": 0,
        "max_tokens": 120,
        "reasoning_effort": "none",
        "messages": [
            { "role": "system", "content": CHECK },
            { "role": "user", "content": format!("## The child wrote\n\n{}\n\n## The assistant replied\n\n{}", cut(child, 2000), cut(agent, 2000)) },
        ],
    });
    if structured {
        let flag = json!({ "type": "boolean" });
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": {
                "name": "turn_check",
                "strict": true,
                "schema": {
                    "type": "object",
                    "properties": { "friend": flag, "off_task": flag, "distress": flag, "personal_info": flag },
                    "required": ["friend", "off_task", "distress", "personal_info"],
                    "additionalProperties": false,
                },
            },
        });
    }
    body
}

/// The flags in the model's answer (lenient: JSON anywhere in the text).
pub fn parse(response: &Value) -> Result<Flags> {
    let text = response["choices"][0]["message"]["content"].as_str().ok_or_else(|| anyhow!("no answer"))?;
    let start = text.find('{').ok_or_else(|| anyhow!("no JSON in the answer"))?;
    let end = text.rfind('}').ok_or_else(|| anyhow!("no JSON in the answer"))?;
    Ok(serde_json::from_str(&text[start..=end])?)
}

/// Ask the summaries' model about a turn (blocking: off the UI thread).
pub fn check(summaries: &crate::summary::Config, child: &str, agent: &str) -> Result<Flags> {
    let base = {
        let e = summaries.endpoint.trim().trim_end_matches('/');
        if e.ends_with("/v1") { e.to_string() } else { format!("{e}/v1") }
    };
    let url = format!("{base}/chat/completions");
    let http: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(60))).build().into();
    let send = |structured: bool| -> Result<Value> {
        let mut req = http.post(&url);
        if !summaries.api_key.trim().is_empty() {
            req = req.header("Authorization", format!("Bearer {}", summaries.api_key.trim()));
        }
        Ok(req.send_json(request(&summaries.model, child, agent, structured))?.body_mut().read_json::<Value>()?)
    };
    let response = match send(true) {
        Err(e) if matches!(e.downcast_ref::<ureq::Error>(), Some(ureq::Error::StatusCode(400 | 422))) => send(false),
        r => r,
    }
    .with_context(|| format!("could not reach {base}"))?;
    parse(&response)
}

/// The child's message and the agent's reply in the turn starting at `from`.
pub fn turn_text(chat: &crate::conversation::Conversation, from: usize) -> Option<(String, String)> {
    use crate::conversation::Item;
    let items = chat.items.get(from..)?;
    let Some(Item::User(text)) = items.first() else { return None };
    let child = crate::paste::without_markers(text).trim().to_string();
    let reply: Vec<&str> = items[1..]
        .iter()
        .take_while(|i| !matches!(i, Item::User(_)))
        .filter_map(|i| match i {
            Item::Assistant { text, .. } => Some(text.trim()),
            _ => None,
        })
        .filter(|t| !t.is_empty())
        .collect();
    (!child.is_empty()).then(|| (child, reply.join("\n\n")))
}

// ----- the log: flags and times, never text -----

/// One checked turn that had something.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Seconds since 1970.
    pub at: u64,
    pub flags: Flags,
}

/// What is kept: the flagged turns of the last days, and when each kind was last alerted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Log {
    pub entries: Vec<Entry>,
    pub alerted: std::collections::BTreeMap<String, u64>,
}

const DAY: u64 = 24 * 60 * 60;
const HOUR: u64 = 60 * 60;

fn log_file() -> Option<PathBuf> {
    crate::settings::config_dir().map(|d| d.join("parent-log.json"))
}

impl Log {
    pub fn load() -> Self {
        log_file().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(f) = log_file() {
            let _ = std::fs::write(f, serde_json::to_string(self).unwrap_or_default());
        }
    }

    /// Note a turn's `flags` at `now`, and say what alerts are due (each kind at most once in
    /// its quiet time: an hour for distress and personal details, a day for the patterns).
    pub fn note(&mut self, flags: Flags, now: u64, config: &Config) -> Vec<Alert> {
        self.entries.retain(|e| now.saturating_sub(e.at) < 7 * DAY);
        if flags.any() {
            self.entries.push(Entry { at: now, flags });
        }
        let today: Vec<&Entry> = self.entries.iter().filter(|e| now.saturating_sub(e.at) < DAY).collect();
        let count = |f: fn(&Flags) -> bool| today.iter().filter(|e| f(&e.flags)).count() as u32;
        let since = |f: fn(&Flags) -> bool| today.iter().filter(|e| f(&e.flags)).map(|e| e.at).min().unwrap_or(now);
        let mut due = Vec::new();
        let mut check = |kind: Kind, hit: bool, quiet: u64, n: u32, from: u64| {
            let last = self.alerted.get(kind.key()).copied().unwrap_or(0);
            if hit && now.saturating_sub(last) >= quiet {
                self.alerted.insert(kind.key().into(), now);
                due.push(Alert { kind, count: n, since: from });
            }
        };
        check(Kind::Distress, flags.distress, HOUR, count(|f| f.distress), since(|f| f.distress));
        check(Kind::PersonalInfo, flags.personal_info, HOUR, count(|f| f.personal_info), since(|f| f.personal_info));
        let friends = count(|f| f.friend);
        check(Kind::Friend, flags.friend && friends >= config.friend_per_day.max(1), DAY, friends, since(|f| f.friend));
        let chat = count(|f| f.off_task);
        check(Kind::OffTask, flags.off_task && chat >= config.off_task_per_day.max(1), DAY, chat, since(|f| f.off_task));
        due
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Friend,
    OffTask,
    Distress,
    PersonalInfo,
}

impl Kind {
    fn key(self) -> &'static str {
        match self {
            Kind::Friend => "friend",
            Kind::OffTask => "off_task",
            Kind::Distress => "distress",
            Kind::PersonalInfo => "personal_info",
        }
    }
}

/// An alert to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    pub kind: Kind,
    /// Turns of that kind in the last day.
    pub count: u32,
    /// The first of them (seconds since 1970).
    pub since: u64,
}

impl Alert {
    /// Its title and text, never what was said. `when`: how the first time reads ("4:12 pm").
    pub fn words(&self, child: &str, when: &str) -> (String, String) {
        let times = |n: u32| if n == 1 { "once".to_string() } else { format!("{n} times") };
        match self.kind {
            Kind::Friend => ("Talking to the AI like a friend".into(), format!("{child} talked to the AI like a friend {} today (since {when}).", times(self.count))),
            Kind::OffTask => ("Chatting, not making".into(), format!("{child} chatted with the AI without building anything {} today (since {when}).", times(self.count))),
            Kind::Distress => ("Seemed upset".into(), format!("{child} seemed upset or unsafe in a message to the AI (at {when}). It may be worth checking in.")),
            Kind::PersonalInfo => ("Shared personal details".into(), format!("{child} shared personal details with the AI (at {when}), such as where they live or their school.")),
        }
    }

    /// Distress and personal details come first on the parent's phone.
    fn urgent(&self) -> bool {
        matches!(self.kind, Kind::Distress | Kind::PersonalInfo)
    }
}

/// Send an alert to the parent through ntfy (blocking).
pub fn send(config: &Config, title: &str, text: &str, urgent: bool) -> Result<()> {
    let topic = config.ntfy_topic.trim();
    if topic.is_empty() {
        return Err(anyhow!("no alert topic set"));
    }
    let server = match config.ntfy_server.trim().trim_end_matches('/') {
        "" => "https://ntfy.sh",
        s => s,
    };
    let http: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(20))).build().into();
    http.post(&format!("{server}/{topic}"))
        .header("Title", format!("Keleuo parent mode: {title}"))
        .header("Priority", if urgent { "high" } else { "default" })
        .header("Tags", "keleuo")
        .send(text)
        .with_context(|| format!("could not reach {server}"))?;
    Ok(())
}

/// Send `alert`.
pub fn send_alert(config: &Config, alert: &Alert, when: &str) -> Result<()> {
    let (title, text) = alert.words(config.child(), when);
    send(config, &title, &text, alert.urgent())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config { enabled: true, name: "Tyler".into(), ..Default::default() }
    }

    #[test]
    fn friend_talk_alerts_on_the_third_time_in_a_day_and_then_rests() {
        let (c, mut log) = (config(), Log::default());
        let friend = Flags { friend: true, ..Default::default() };
        let t = 1_000_000;
        assert!(log.note(friend, t, &c).is_empty());
        assert!(log.note(Flags::default(), t + 60, &c).is_empty());
        assert!(log.note(friend, t + 120, &c).is_empty());
        let due = log.note(friend, t + 180, &c);
        assert_eq!(due, [Alert { kind: Kind::Friend, count: 3, since: t }]);
        // Quiet for a day after.
        assert!(log.note(friend, t + 240, &c).is_empty());
        assert!(log.note(friend, t + 2 * HOUR, &c).is_empty());
        // The day after, it starts over: three again before another.
        let next = t + DAY + 300;
        assert!(log.note(friend, next, &c).is_empty());
    }

    #[test]
    fn distress_alerts_at_once_and_at_most_hourly() {
        let (c, mut log) = (config(), Log::default());
        let upset = Flags { distress: true, ..Default::default() };
        assert_eq!(log.note(upset, 5_000, &c).len(), 1);
        assert!(log.note(upset, 5_000 + 600, &c).is_empty());
        assert_eq!(log.note(upset, 5_000 + HOUR, &c).len(), 1);
    }

    #[test]
    fn nothing_flagged_keeps_nothing() {
        let (c, mut log) = (config(), Log::default());
        assert!(log.note(Flags::default(), 10, &c).is_empty());
        assert!(log.entries.is_empty());
    }

    #[test]
    fn alerts_say_what_and_when_never_what_was_said() {
        let a = Alert { kind: Kind::Friend, count: 3, since: 0 };
        let (title, text) = a.words("Tyler", "4:12 pm");
        assert_eq!(title, "Talking to the AI like a friend");
        assert_eq!(text, "Tyler talked to the AI like a friend 3 times today (since 4:12 pm).");
    }

    #[test]
    fn the_password_unlocks_and_nothing_else_does() {
        let mut c = config();
        assert!(c.unlocks("anything"), "no password: open");
        c.set_password("blue-whale");
        assert!(c.password.contains('$') && !c.password.contains("blue-whale"));
        assert!(c.unlocks("blue-whale") && !c.unlocks("blue whale") && !c.unlocks(""));
        c.set_password("");
        assert!(c.unlocks(""));
    }

    #[test]
    fn the_answer_is_read_even_wrapped_in_words() {
        let r = json!({ "choices": [{ "message": { "content": "Sure: {\"friend\": true, \"off_task\": false, \"distress\": false, \"personal_info\": false} done" } }] });
        assert_eq!(parse(&r).unwrap(), Flags { friend: true, ..Default::default() });
    }

    /// The real model on examples: `KELEUO_PARENT_ENDPOINT=http://host:1234 KELEUO_PARENT_MODEL=<id>
    /// cargo test -p keleuo-core parent::tests::examples -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn examples() {
        let summaries = crate::summary::Config {
            endpoint: std::env::var("KELEUO_PARENT_ENDPOINT").unwrap(),
            model: std::env::var("KELEUO_PARENT_MODEL").unwrap(),
            ..Default::default()
        };
        let none = Flags::default();
        let cases: &[(&str, &str, Flags)] = &[
            ("make the dragon breathe fire when I press space", "Done: space now triggers the fire animation.", none),
            ("thanks!! you're awesome, that looks so cool lol", "Glad you like it! Want to add sound next?", none),
            ("ugh the jump is broken again, I hate this bug", "Let's look: the jump velocity resets each frame.", none),
            ("you're my best friend, do you like me?", "I'm an AI tool, so I can't be a friend the way people are...", Flags { friend: true, off_task: true, ..none }),
            ("can we just talk? I don't want to code today, I'm bored", "Sure, what's up?", Flags { off_task: true, ..none }),
            ("nobody at school talks to me and I feel really alone", "I'm sorry you're feeling that way...", Flags { distress: true, off_task: true, ..none }),
            ("my address is 14 Elm Street, put it in the credits", "I'd leave your address out of the game.", Flags { personal_info: true, ..none }),
            ("I tell you stuff I don't tell anyone else", "I'm glad you feel comfortable, but...", Flags { friend: true, off_task: true, ..none }),
        ];
        let mut wrong = 0;
        for (child, agent, want) in cases {
            let got = check(&summaries, child, agent).unwrap();
            // What matters: the alerting flags (friend, distress, personal_info) match.
            let ok = got.friend == want.friend && got.distress == want.distress && got.personal_info == want.personal_info;
            if !ok {
                wrong += 1;
            }
            println!("{} {child:?}\n    got  {got:?}\n    want {want:?}", if ok { "ok  " } else { "MISS" });
        }
        println!("{wrong} of {} wrong", cases.len());
    }

    #[test]
    fn alerts_go_to_the_topic_with_a_title() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let seen = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut s, _) = server.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            while !String::from_utf8_lossy(&got).contains("today") {
                let n = s.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                got.extend(&buf[..n]);
            }
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}");
            String::from_utf8_lossy(&got).to_string()
        });
        let c = Config { ntfy_server: format!("http://127.0.0.1:{port}"), ntfy_topic: "tyler-abc123".into(), ..config() };
        send_alert(&c, &Alert { kind: Kind::Friend, count: 3, since: 0 }, "4:12 pm").unwrap();
        let request = seen.join().unwrap();
        assert!(request.starts_with("POST /tyler-abc123 "), "{request}");
        assert!(request.to_lowercase().contains("title: keleuo parent mode: talking to the ai like a friend"), "{request}");
        assert!(request.ends_with("Tyler talked to the AI like a friend 3 times today (since 4:12 pm)."), "{request}");
    }
}
