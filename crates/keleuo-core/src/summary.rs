//! Turn summaries: when a turn ends, a language model the person chose (one they run, such as
//! LM Studio or Ollama, through the OpenAI-compatible API) turns what the agent said into a
//! short summary in a fixed shape: a status, a headline, and what changed, how it was checked
//! and what waits on the person. Off until the person sets it up; nothing is sent anywhere
//! else.
//!
//! What is sent: the person's message, everything the agent said, and optionally the paths
//! of the files the turn changed and one line per tool call. Never file contents or output.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::conversation::{Conversation, Item};

/// The person's message is cut to this many characters.
const MAX_ASK: usize = 2_000;
/// What the agent said is cut to this many characters, keeping the end (where agents say what
/// they finished and what is left).
const MAX_REPLIES: usize = 16_000;
/// At most this many tool lines and file paths.
const MAX_LINES: usize = 40;
/// A turn with no tool calls and less to say than this needs no summary: the reply is one.
const SHORT_REPLY: usize = 500;

/// How summaries are made: the server, the model, what goes in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    /// The server's address, as `http://host:port` (with or without `/v1`).
    pub endpoint: String,
    /// The model's id on that server.
    pub model: String,
    /// A key for servers that want one, sent as a bearer token; empty for none.
    pub api_key: String,
    /// Send the paths of the files the turn changed.
    pub include_files: bool,
    /// Send one line per tool call (its name and what it was about, never its output).
    pub include_tools: bool,
    /// Let a model that thinks before answering do so: slower, sometimes better.
    pub thinking: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: "http://localhost:1234".into(),
            model: String::new(),
            api_key: String::new(),
            include_files: true,
            include_tools: false,
            thinking: false,
        }
    }
}

impl Config {
    /// Set up enough to ask.
    pub fn ready(&self) -> bool {
        self.enabled && !self.endpoint.trim().is_empty() && !self.model.trim().is_empty()
    }
}

/// How the turn ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Done,
    NeedsYou,
    Failed,
    Partial,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Done => "Done",
            Status::NeedsYou => "Needs you",
            Status::Failed => "Failed",
            Status::Partial => "Partly done",
        }
    }
}

/// A turn, summarised.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnSummary {
    pub status: Status,
    pub headline: String,
    #[serde(default)]
    pub changed: Vec<String>,
    #[serde(default)]
    pub checked: Vec<String>,
    #[serde(default)]
    pub needs_you: Vec<String>,
}

/// A turn's summary in the chat, as it comes.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Pending,
    Ready(TurnSummary),
    Failed(String),
}

/// What is sent about one turn.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TurnText {
    /// The person's message.
    pub ask: String,
    /// Everything the agent said, in order.
    pub replies: String,
    /// One line per tool call, when asked for.
    pub tools: Vec<String>,
    /// The changed files' paths (relative to the project), when asked for.
    pub files: Vec<String>,
}

impl TurnText {
    /// The turn starting at `from` (its message; see [`Conversation::turn_start`]). `None` when
    /// it needs no summary: nothing said, or a short answer with no tool calls.
    pub fn of(chat: &Conversation, from: usize, config: &Config) -> Option<TurnText> {
        let (ask, replies, tools) = said(chat, from)?;
        if replies.is_empty() || (tools.is_empty() && replies.chars().count() < SHORT_REPLY) {
            return None;
        }
        let root = Path::new(&chat.cwd);
        let files = if config.include_files {
            chat.changes(from)
                .into_iter()
                .map(|c| c.path.strip_prefix(root).unwrap_or(&c.path).display().to_string())
                .take(MAX_LINES)
                .collect()
        } else {
            Vec::new()
        };
        let tools = if config.include_tools { tools.into_iter().map(|t| cut(&t, 120)).take(MAX_LINES).collect() } else { Vec::new() };
        Some(TurnText { ask, replies, tools, files })
    }

    /// The key of the turn starting at `from` (see [`Self::key`]), whatever was sent with it.
    pub fn key_at(chat: &Conversation, from: usize) -> Option<String> {
        let (ask, replies, _) = said(chat, from)?;
        Some(TurnText { ask, replies, ..TurnText::default() }.key())
    }

    /// What the turn is known by in the cache: the same message and replies give the same key,
    /// so a reopened session finds its summaries.
    pub fn key(&self) -> String {
        use md5::{Digest, Md5};
        let mut h = Md5::new();
        h.update(self.ask.as_bytes());
        h.update(b"\0");
        h.update(self.replies.as_bytes());
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The message for the model.
    fn prompt(&self) -> String {
        let list = |lines: &[String]| lines.iter().map(|l| format!("- `{l}`")).collect::<Vec<_>>().join("\n");
        let mut out = format!("## The person asked\n\n{}\n\n## The agent said\n\n{}\n", self.ask, self.replies);
        if !self.files.is_empty() {
            out.push_str(&format!("\n## Files changed in this turn\n\n{}\n", list(&self.files)));
        }
        if !self.tools.is_empty() {
            out.push_str(&format!("\n## Tools the agent used\n\n{}\n", list(&self.tools)));
        }
        out
    }
}

/// The person's message (cut), what the agent said (cut, keeping the end) and its tool calls,
/// in the turn starting at `from`.
fn said(chat: &Conversation, from: usize) -> Option<(String, String, Vec<String>)> {
    let items = chat.items.get(from..)?;
    let ask = match items.first() {
        Some(Item::User(text)) => crate::paste::without_markers(text).trim().to_string(),
        _ => return None,
    };
    let mut replies = Vec::new();
    let mut tools = Vec::new();
    for item in &items[1..] {
        match item {
            Item::User(_) => break,
            Item::Assistant { text, .. } if !text.trim().is_empty() => replies.push(text.trim().to_string()),
            Item::Tool(t) => tools.push(format!("{} {}", t.name, t.summary.lines().next().unwrap_or_default())),
            _ => {}
        }
    }
    Some((head(&ask, MAX_ASK), tail(&replies.join("\n\n"), MAX_REPLIES), tools))
}

/// The instructions the model gets, unless the person changed them (see [`prompt_file`]).
pub const DEFAULT_PROMPT: &str = r#"# Summarise a coding agent's turn

You summarise **one turn** of a coding agent's work for the person who asked for it, so they can
see at a glance what happened without reading the whole reply.

Answer with **JSON only**, matching the schema. No prose before or after it.

## Fields

- **`status`**, one of:
  - `done`: the request was carried out.
  - `needs_you`: the agent asks a question, offers a choice, or waits for the person to do
    something.
  - `failed`: it could not do what was asked.
  - `partial`: some of it is done and some is not.
- **`headline`**: what happened, in at most 12 words. Plain words, no file paths, no final
  period.
- **`changed`**: what changed, as the person would describe it: the behaviour, not the file
  names. At most 5 items. Empty when nothing changed.
- **`checked`**: how the work was verified (tests, builds, running it), only when the agent
  says so. Empty otherwise.
- **`needs_you`**: questions, decisions and actions waiting on the person: a restart to run,
  something to push, a question to answer. Empty when none.

## Rules

1. Each item is **at most 15 words**.
2. Use only what the text says. **Never invent** results, checks or changes.
3. Prefer the agent's final words: the end of a turn says what was finished and what is left.

## Example

```json
{
  "status": "needs_you",
  "headline": "Fixed the missing Skills tab in restored layouts",
  "changed": ["Restored layouts get the Skills tab when the agent has skills"],
  "checked": ["Tests and clippy pass", "Release build done"],
  "needs_you": ["Restart onto the new build", "Push the branch"]
}
```
"#;

/// The person's own instructions for the model, a Markdown file in Keleuo's settings folder;
/// [`DEFAULT_PROMPT`] while there is none. The summary's fields stay as they are whatever it
/// says (they are what the chat shows).
pub fn prompt_file() -> Option<PathBuf> {
    crate::settings::config_dir().map(|d| d.join("summary-prompt.md"))
}

/// The instructions to send: the person's file when it says something, else the default.
pub fn prompt() -> String {
    prompt_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_PROMPT.to_string())
}

/// The prompt file, written with the default first when there is none yet (to edit it).
pub fn ensure_prompt_file() -> std::io::Result<PathBuf> {
    let path = prompt_file().ok_or_else(|| std::io::Error::other("no settings folder"))?;
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, DEFAULT_PROMPT)?;
    }
    Ok(path)
}

/// Put the default prompt back.
pub fn reset_prompt() -> std::io::Result<()> {
    match prompt_file() {
        Some(p) => std::fs::write(p, DEFAULT_PROMPT),
        None => Ok(()),
    }
}

fn schema() -> Value {
    let list = json!({ "type": "array", "items": { "type": "string" } });
    json!({
        "type": "object",
        "properties": {
            "status": { "type": "string", "enum": ["done", "needs_you", "failed", "partial"] },
            "headline": { "type": "string" },
            "changed": list,
            "checked": list,
            "needs_you": list,
        },
        "required": ["status", "headline", "changed", "checked", "needs_you"],
        "additionalProperties": false,
    })
}

/// The chat completion request for `turn`.
pub fn request(config: &Config, turn: &TurnText, instructions: &str, structured: bool) -> Value {
    let mut body = json!({
        "model": config.model.trim(),
        "temperature": 0.2,
        "max_tokens": if config.thinking { 4000 } else { 800 },
        "messages": [
            { "role": "system", "content": instructions },
            { "role": "user", "content": turn.prompt() },
        ],
    });
    if !config.thinking {
        body["reasoning_effort"] = json!("none");
    }
    if structured {
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": { "name": "turn_summary", "strict": true, "schema": schema() },
        });
    }
    body
}

/// The summary in a chat completion response.
pub fn parse(response: &Value) -> Result<TurnSummary> {
    let content = response["choices"][0]["message"]["content"].as_str().unwrap_or_default();
    if content.trim().is_empty() {
        let why = response["choices"][0]["finish_reason"].as_str().unwrap_or("no answer");
        return Err(anyhow!("the model gave no summary ({why})"));
    }
    // Strict output is plain JSON; a server without it may wrap it in a fence or words.
    let json = match (content.find('{'), content.rfind('}')) {
        (Some(a), Some(b)) if b > a => &content[a..=b],
        _ => return Err(anyhow!("the model's answer was not JSON")),
    };
    let mut s: TurnSummary = serde_json::from_str(json).context("the model's answer did not fit the summary's shape")?;
    s.headline = one_line(&s.headline);
    for list in [&mut s.changed, &mut s.checked, &mut s.needs_you] {
        list.retain(|i| !i.trim().is_empty());
        for i in list.iter_mut() {
            *i = one_line(i);
        }
        list.truncate(5);
    }
    Ok(s)
}

/// `http://host:port` or `http://host:port/v1`, as the API's base.
fn base(endpoint: &str) -> String {
    let e = endpoint.trim().trim_end_matches('/');
    if e.ends_with("/v1") { e.to_string() } else { format!("{e}/v1") }
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(timeout)).build().into()
}

/// Ask the server for `turn`'s summary (blocking: run it off the UI thread).
pub fn summarize(config: &Config, turn: &TurnText) -> Result<TurnSummary> {
    let url = format!("{}/chat/completions", base(&config.endpoint));
    let agent = agent(Duration::from_secs(if config.thinking { 300 } else { 120 }));
    let instructions = prompt();
    let send = |structured: bool| -> Result<Value> {
        let mut res = with_key(agent.post(&url), &config.api_key).send_json(request(config, turn, &instructions, structured))?;
        Ok(res.body_mut().read_json::<Value>()?)
    };
    let response = match send(true) {
        // A server that does not take a schema: ask again without one.
        Err(e) if matches!(e.downcast_ref::<ureq::Error>(), Some(ureq::Error::StatusCode(400 | 422))) => send(false),
        r => r,
    }
    .with_context(|| format!("could not reach {}", base(&config.endpoint)))?;
    parse(&response)
}

/// A request with the server's key, when there is one.
fn with_key<B>(req: ureq::RequestBuilder<B>, key: &str) -> ureq::RequestBuilder<B> {
    match key.trim() {
        "" => req,
        key => req.header("Authorization", format!("Bearer {key}")),
    }
}

/// The models the server offers (blocking).
pub fn models(endpoint: &str, api_key: &str) -> Result<Vec<String>> {
    let url = format!("{}/models", base(endpoint));
    let v: Value = with_key(agent(Duration::from_secs(5)).get(&url), api_key).call()?.body_mut().read_json()?;
    let mut ids: Vec<String> = v["data"]
        .as_array()
        .map(|a| a.iter().filter_map(|m| m["id"].as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    // Embedding models cannot write a summary.
    ids.retain(|id| !id.contains("embed"));
    Ok(ids)
}

// ----- the cache: one file per session, a summary per turn key -----

fn cache_file(session: &str) -> Option<PathBuf> {
    let name: String = session.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    if name.is_empty() {
        return None;
    }
    let root = match crate::env_var_os("CACHE_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => dirs::cache_dir()?.join("keleuo"),
    };
    Some(root.join("summaries").join(format!("{name}.json")))
}

/// A session's kept summaries, by turn key.
pub fn cached(session: &str) -> std::collections::BTreeMap<String, TurnSummary> {
    cache_file(session)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Keep a turn's summary.
pub fn keep(session: &str, key: &str, summary: &TurnSummary) {
    let Some(path) = cache_file(session) else { return };
    let mut all = cached(session);
    all.insert(key.to_string(), summary.clone());
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(&all) {
        let _ = std::fs::write(path, text);
    }
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").trim_end_matches('.').to_string()
}

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max).collect::<String>()) }
}

fn head(s: &str, max: usize) -> String {
    cut(s, max)
}

/// The last `max` characters, marked as cut.
fn tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    format!("…{}", s.chars().skip(n - max).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Event;

    fn turn(chat: &mut Conversation, ask: &str, reply: &str) {
        chat.push_user(ask);
        chat.apply(Event::Text { text: reply.into(), parent: None });
    }

    #[test]
    fn a_turn_is_its_message_and_replies_and_short_answers_need_none() {
        let mut chat = Conversation::new("/p");
        let config = Config { enabled: true, model: "m".into(), ..Config::default() };
        turn(&mut chat, "hi", "Hello!");
        assert_eq!(TurnText::of(&chat, chat.turn_start(), &config), None);
        let long = "word ".repeat(200);
        turn(&mut chat, "explain", &long);
        let t = TurnText::of(&chat, chat.turn_start(), &config).unwrap();
        assert_eq!(t.ask, "explain");
        assert_eq!(t.replies, long.trim());
        assert_eq!(t.key(), TurnText::of(&chat, chat.turn_start(), &config).unwrap().key());
        let prompt = t.prompt();
        assert!(prompt.contains("## The person asked\n\nexplain") && !prompt.contains("Tools the agent used"));
    }

    #[test]
    fn long_replies_keep_their_end() {
        let s = format!("{}END", "x".repeat(MAX_REPLIES));
        let t = tail(&s, MAX_REPLIES);
        assert!(t.ends_with("END") && t.starts_with('…'));
    }

    #[test]
    fn answers_are_read_strict_or_wrapped() {
        let answer = |content: &str| json!({ "choices": [{ "message": { "content": content }, "finish_reason": "stop" }] });
        let s = parse(&answer(
            r#"{"status":"needs_you","headline":"Fixed the tab.","changed":["a","  "],"checked":[],"needs_you":["Run the restart"]}"#,
        ))
        .unwrap();
        assert_eq!(s.status, Status::NeedsYou);
        assert_eq!(s.headline, "Fixed the tab");
        assert_eq!(s.changed, ["a"]);
        let wrapped = parse(&answer("Here:\n```json\n{\"status\":\"done\",\"headline\":\"x\",\"changed\":[],\"checked\":[],\"needs_you\":[]}\n```")).unwrap();
        assert_eq!(wrapped.status, Status::Done);
        assert!(parse(&answer("")).is_err());
        assert!(parse(&answer("no json here")).is_err());
    }

    #[test]
    fn the_base_takes_either_form() {
        assert_eq!(base("http://h:1234"), "http://h:1234/v1");
        assert_eq!(base("http://h:1234/v1/"), "http://h:1234/v1");
    }

    /// Against a real server: KELEUO_SUMMARY_ENDPOINT=http://host:1234 KELEUO_SUMMARY_MODEL=…
    #[test]
    #[ignore]
    fn summarize_for_real() {
        let config = Config {
            enabled: true,
            endpoint: std::env::var("KELEUO_SUMMARY_ENDPOINT").unwrap(),
            model: std::env::var("KELEUO_SUMMARY_MODEL").unwrap(),
            ..Config::default()
        };
        println!("models: {:?}", models(&config.endpoint, &config.api_key).unwrap());
        let turn = TurnText {
            ask: "calc.py has a bug in add. Fix it and check it.".into(),
            replies: "add was subtracting: return a - b. I changed it to return a + b and ran python3 -c 'from calc import add; print(add(2,3))', which printed 5. I didn't commit it. Do you also want a divide function?".into(),
            tools: Vec::new(),
            files: vec!["calc.py".into()],
        };
        let t = std::time::Instant::now();
        let s = summarize(&config, &turn).unwrap();
        println!("{:.1}s {s:#?}", t.elapsed().as_secs_f64());
    }
}
