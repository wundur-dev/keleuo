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

/// How the work went (whether something waits on the person is apart: [`TurnSummary::needs_you`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Done,
    Partial,
    Failed,
    /// The agent stopped on a question and cannot go on without an answer (also read from
    /// summaries kept before there was a list of what needs the person).
    #[serde(alias = "needs_you")]
    Waiting,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Done => "Done",
            Status::Partial => "Partly done",
            Status::Failed => "Failed",
            Status::Waiting => "Waiting on you",
        }
    }
}

/// How a check came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Passed,
    Failed,
    NotRun,
    /// The agent did not say.
    Unknown,
}

/// Where in the agent's reply an item comes from: the piece's text (to find it in the chat)
/// and, for a `!` command block, the command (to run it).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub text: String,
    #[serde(default)]
    pub command: Option<String>,
}

/// How the work was checked, and how that came out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "CheckIn")]
pub struct Check {
    pub what: String,
    pub outcome: Outcome,
    pub source: Option<Source>,
}

/// Something waiting on the person.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "TodoIn")]
pub struct Todo {
    pub text: String,
    pub source: Option<Source>,
}

/// A check as kept: in full, or a plain line (kept before checks had outcomes).
#[derive(Deserialize)]
#[serde(untagged)]
enum CheckIn {
    Full { what: String, outcome: Outcome, #[serde(default)] source: Option<Source> },
    Plain(String),
}

impl From<CheckIn> for Check {
    fn from(c: CheckIn) -> Self {
        match c {
            CheckIn::Full { what, outcome, source } => Check { what, outcome, source },
            CheckIn::Plain(what) => Check { what, outcome: Outcome::Unknown, source: None },
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TodoIn {
    Full { text: String, #[serde(default)] source: Option<Source> },
    Plain(String),
}

impl From<TodoIn> for Todo {
    fn from(t: TodoIn) -> Self {
        match t {
            TodoIn::Full { text, source } => Todo { text, source },
            TodoIn::Plain(text) => Todo { text, source: None },
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
    pub checked: Vec<Check>,
    #[serde(default)]
    pub needs_you: Vec<Todo>,
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
    /// Everything the agent said, in order (the cache key is made of it).
    pub replies: String,
    /// The same, in tagged pieces, as the model gets it (see [`pieces`]).
    pub pieces: Vec<Piece>,
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
        let pieces = pieces(&replies);
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
        Some(TurnText { ask, replies, pieces, tools, files })
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
        let said: Vec<String> = self.pieces.iter().map(|p| format!("[{}] {}", p.tag, p.text)).collect();
        let mut out = format!("## The person asked\n\n{}\n\n## The agent said\n\n{}\n", self.ask, said.join("\n\n"));
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

/// A piece of what the agent said, tagged so the model can point at it: `P3` a paragraph or
/// list item, `C1` a `!` command block (only those can be run from the summary).
#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    pub tag: String,
    pub text: String,
    pub command: Option<String>,
}

/// Cut `replies` into pieces: paragraphs, list items and code blocks (a fence is kept whole).
/// Numbered from the start; when there is too much, the earliest go (the end says what was
/// finished and what is left).
pub fn pieces(replies: &str) -> Vec<Piece> {
    let mut blocks: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut fence: Option<&str> = None;
    let flush = |cur: &mut Vec<&str>, blocks: &mut Vec<String>| {
        let text = cur.join("\n").trim().to_string();
        if !text.is_empty() {
            blocks.push(text);
        }
        cur.clear();
    };
    for line in replies.lines() {
        let t = line.trim_start();
        if let Some(open) = fence {
            cur.push(line);
            if t.starts_with(open) {
                fence = None;
                flush(&mut cur, &mut blocks);
            }
            continue;
        }
        if t.starts_with("```") || t.starts_with("~~~") {
            flush(&mut cur, &mut blocks);
            fence = Some(if t.starts_with("```") { "```" } else { "~~~" });
            cur.push(line);
            continue;
        }
        if t.is_empty() {
            flush(&mut cur, &mut blocks);
            continue;
        }
        let indent = line.len() - t.len();
        let item = t.starts_with("- ") || t.starts_with("* ") || t.split_once(". ").is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if item && indent < 4 {
            flush(&mut cur, &mut blocks);
        }
        cur.push(line);
    }
    flush(&mut cur, &mut blocks);
    let (mut p, mut c) = (0, 0);
    let mut out: Vec<Piece> = blocks
        .into_iter()
        .map(|text| {
            let command = fenced_command(&text);
            let tag = if command.is_some() {
                c += 1;
                format!("C{c}")
            } else {
                p += 1;
                format!("P{p}")
            };
            Piece { tag, text, command }
        })
        .collect();
    let mut total: usize = out.iter().map(|p| p.text.chars().count()).sum();
    while total > MAX_REPLIES && out.len() > 1 {
        total -= out.remove(0).text.chars().count();
    }
    if let Some(only) = out.first_mut().filter(|_| total > MAX_REPLIES) {
        only.text = tail(&only.text, MAX_REPLIES);
    }
    out
}

/// A fenced block holding one `!` command: the command.
fn fenced_command(block: &str) -> Option<String> {
    let lines: Vec<&str> = block.lines().collect();
    let fence = |l: &str| l.trim_start().starts_with("```") || l.trim_start().starts_with("~~~");
    if lines.len() < 3 || !fence(lines[0]) || !fence(lines[lines.len() - 1]) {
        return None;
    }
    crate::conversation::shell_command(&lines[1..lines.len() - 1].join("\n"))
}

/// What the model is told about the job, unless the person changed it (see [`prompt_file`]):
/// who it writes for, and how. What the answer's fields mean is in [`FORMAT`], which always
/// follows, so an edited (or older) prompt cannot change it.
pub const DEFAULT_PROMPT: &str = r#"# Summarise a coding agent's turn

You summarise **one turn** of a coding agent's work for the person who asked for it, so they can
see at a glance what happened without reading the whole reply.

## Rules

1. Each item is **at most 15 words**. The headline is at most 12.
2. Plain words. Say what changed as the person would describe it: the behaviour, not the file
   names.
3. Use only what the text says. **Never invent** results, checks or changes.
4. Prefer the agent's final words: the end of a turn says what was finished and what is left.
"#;

/// The answer's shape: what each field means, the tags, an example. Always sent after the
/// prompt; it goes with the schema, which the person cannot change either.
const FORMAT: &str = r#"## The answer

Answer with **JSON only**, matching the schema. No prose before or after it.

What the agent said comes in tagged pieces: `[P3]` is a paragraph or list item, `[C1]` a
command the agent gave the person to run. Use the tags to point at where an item comes from.

- **`status`**: how the work went, one of:
  - `done`: what was asked is carried out (even when there are steps left for the person).
  - `partial`: some of it is done and some is not.
  - `failed`: it could not do what was asked.
  - `waiting`: the agent stopped on a question and cannot go on without an answer.
- **`headline`**: what happened. No file paths, no final period.
- **`changed`**: what changed, at most 5 items. Empty when nothing changed.
- **`checked`**: how the work was verified, only when the agent says so. Each has:
  - `what`: the check (tests, a build, running it), in a few words.
  - `outcome`: how the check came out:
    - `passed`: the agent says it worked, passed, is clean, or showed what was expected.
    - `failed`: the agent says it failed, errored, or showed something wrong.
    - `not_run`: the agent says it did not run it.
    - `unknown`: the agent does not say how it came out. Never assume either way.
  - `source`: the tag of the piece that reports it, or `""`.
- **`needs_you`**: questions, decisions and actions waiting on the person: a command to run,
  something to push, a question to answer. Each has:
  - `text`: what to do or answer, in a few words.
  - `source`: the tag of the piece it comes from (a `C` tag when there is a command for it),
    or `""`.

  One item per thing to do. A command belongs to its item as its `source`: never list the
  command itself, or what it should print, as an item of its own.

## Example

```json
{
  "status": "done",
  "headline": "Fixed the missing Skills tab in restored layouts",
  "changed": ["Restored layouts get the Skills tab when the agent has skills"],
  "checked": [
    { "what": "Unit tests", "outcome": "passed", "source": "P4" },
    { "what": "In the app", "outcome": "not_run", "source": "P5" }
  ],
  "needs_you": [{ "text": "Restart onto the new build", "source": "C1" }]
}
```
"#;

/// Earlier defaults (md5 of their trimmed text): a prompt file that is still one of them was
/// never edited, and gets the current default.
const OLD_DEFAULTS: [&str; 3] =
    ["219c0cbaa4d4d905a01281a5291594a7", "24bc6a60862379ab119f26e0a1c37993", "74ab7eda124f7394adef0f6832d0397b"];

fn md5_of(text: &str) -> String {
    use md5::{Digest, Md5};
    Md5::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// The person's own instructions for the model, a Markdown file in Keleuo's settings folder;
/// [`DEFAULT_PROMPT`] while there is none. The summary's fields stay as they are whatever it
/// says (they are what the chat shows).
pub fn prompt_file() -> Option<PathBuf> {
    crate::settings::config_dir().map(|d| d.join("summary-prompt.md"))
}

/// The person's prompt: their file when it says something of their own, else the default.
pub fn prompt() -> String {
    prompt_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .filter(|t| !t.trim().is_empty() && !OLD_DEFAULTS.contains(&md5_of(t.trim()).as_str()))
        .unwrap_or_else(|| DEFAULT_PROMPT.to_string())
}

/// What the model is told: the prompt, then the answer's shape.
pub fn instructions() -> String {
    format!("{}\n\n{FORMAT}", prompt().trim_end())
}

/// The prompt file, written with the default first when there is none yet, or it is an
/// earlier default never edited (to edit it).
pub fn ensure_prompt_file() -> std::io::Result<PathBuf> {
    let path = prompt_file().ok_or_else(|| std::io::Error::other("no settings folder"))?;
    let old = std::fs::read_to_string(&path).is_ok_and(|t| OLD_DEFAULTS.contains(&md5_of(t.trim()).as_str()));
    if !path.exists() || old {
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

fn schema(tags: &[String]) -> Value {
    let mut sources: Vec<Value> = tags.iter().map(|t| json!(t)).collect();
    sources.push(json!(""));
    let source = json!({ "type": "string", "enum": sources });
    json!({
        "type": "object",
        "properties": {
            "status": { "type": "string", "enum": ["done", "partial", "failed", "waiting"] },
            "headline": { "type": "string" },
            "changed": { "type": "array", "items": { "type": "string" } },
            "checked": { "type": "array", "items": {
                "type": "object",
                "properties": {
                    "what": { "type": "string" },
                    "outcome": { "type": "string", "enum": ["passed", "failed", "not_run", "unknown"] },
                    "source": source,
                },
                "required": ["what", "outcome", "source"],
                "additionalProperties": false,
            } },
            "needs_you": { "type": "array", "items": {
                "type": "object",
                "properties": { "text": { "type": "string" }, "source": source },
                "required": ["text", "source"],
                "additionalProperties": false,
            } },
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
            "json_schema": {
                "name": "turn_summary",
                "strict": true,
                "schema": schema(&turn.pieces.iter().map(|p| p.tag.clone()).collect::<Vec<_>>()),
            },
        });
    }
    body
}

/// The model's answer, before its tags are turned into sources. Lenient: a server without
/// structured output may give plain lines, or no tags.
#[derive(Deserialize)]
struct Answer {
    status: Status,
    headline: String,
    #[serde(default)]
    changed: Vec<String>,
    #[serde(default)]
    checked: Vec<AnswerCheck>,
    #[serde(default)]
    needs_you: Vec<AnswerTodo>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AnswerCheck {
    Full { what: String, #[serde(default = "unknown")] outcome: Outcome, #[serde(default)] source: String },
    Plain(String),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AnswerTodo {
    Full { text: String, #[serde(default)] source: String },
    Plain(String),
}

fn unknown() -> Outcome {
    Outcome::Unknown
}

/// The summary in a chat completion response; `pieces` are what the tags point at (a tag that
/// is not one of them points nowhere).
pub fn parse(response: &Value, pieces: &[Piece]) -> Result<TurnSummary> {
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
    let a: Answer = serde_json::from_str(json).context("the model's answer did not fit the summary's shape")?;
    let source = |tag: &str| {
        pieces.iter().find(|p| p.tag == tag.trim().trim_matches(['[', ']'])).map(|p| Source { text: p.text.clone(), command: p.command.clone() })
    };
    let lines = |list: Vec<String>| list.into_iter().map(|l| one_line(&l)).filter(|l| !l.is_empty()).take(5).collect::<Vec<_>>();
    let checked = a
        .checked
        .into_iter()
        .map(|c| match c {
            AnswerCheck::Full { what, outcome, source: tag } => Check { what: one_line(&what), outcome, source: source(&tag) },
            AnswerCheck::Plain(what) => Check { what: one_line(&what), outcome: Outcome::Unknown, source: None },
        })
        .filter(|c| !c.what.is_empty())
        .take(5)
        .collect();
    let needs_you = a
        .needs_you
        .into_iter()
        .map(|t| match t {
            AnswerTodo::Full { text, source: tag } => Todo { text: one_line(&text), source: source(&tag) },
            AnswerTodo::Plain(text) => Todo { text: one_line(&text), source: None },
        })
        .filter(|t| !t.text.is_empty())
        .collect();
    let needs_you = fold_commands(needs_you, pieces).into_iter().take(5).collect();
    Ok(TurnSummary { status: a.status, headline: one_line(&a.headline), changed: lines(a.changed), checked, needs_you })
}

/// An item that is only one of the agent's commands written out joins the item before it, as
/// that item's source (small models list the command apart from what it is for).
fn fold_commands(items: Vec<Todo>, pieces: &[Piece]) -> Vec<Todo> {
    let squash = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>();
    let mut out: Vec<Todo> = Vec::new();
    for t in items {
        let bare = squash(&t.text);
        let command = pieces.iter().find(|p| p.command.as_deref().is_some_and(|c| !bare.is_empty() && squash(c) == bare));
        match (command, out.last_mut()) {
            (Some(p), Some(prev)) => {
                if prev.source.as_ref().is_none_or(|s| s.command.is_none()) {
                    prev.source = Some(Source { text: p.text.clone(), command: p.command.clone() });
                }
            }
            _ => out.push(t),
        }
    }
    out
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
    let instructions = instructions();
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
    parse(&response, &turn.pieces)
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
    fn replies_are_cut_into_tagged_pieces_and_bang_blocks_are_commands() {
        let reply = "First paragraph\nstill first.\n\n- one\n- two\n\n```\n! ./scripts/restart\n```\n\n```rust\nfn x() {}\n\nfn y() {}\n```\nLast.";
        let p = pieces(reply);
        let tags: Vec<&str> = p.iter().map(|p| p.tag.as_str()).collect();
        assert_eq!(tags, ["P1", "P2", "P3", "C1", "P4", "P5"]);
        assert_eq!(p[0].text, "First paragraph\nstill first.");
        assert_eq!(p[3].command.as_deref(), Some("./scripts/restart"));
        assert!(p[4].text.contains("fn y()"), "a fence is kept whole, blank lines and all");
        assert_eq!(p[4].command, None);
    }

    #[test]
    fn answers_are_read_with_their_sources_strict_or_wrapped() {
        let answer = |content: &str| json!({ "choices": [{ "message": { "content": content }, "finish_reason": "stop" }] });
        let p = pieces("Tests pass.\n\n```\n! ./scripts/restart\n```");
        let s = parse(
            &answer(
                r#"{"status":"done","headline":"Fixed the tab.","changed":["a","  "],
                "checked":[{"what":"tests","outcome":"passed","source":"P1"},{"what":"clippy","outcome":"unknown","source":""}],
                "needs_you":[{"text":"Restart","source":"C1"},{"text":"Push","source":"P9"}]}"#,
            ),
            &p,
        )
        .unwrap();
        assert_eq!(s.status, Status::Done);
        assert_eq!(s.headline, "Fixed the tab");
        assert_eq!(s.changed, ["a"]);
        assert_eq!(s.checked[0].outcome, Outcome::Passed);
        assert_eq!(s.checked[0].source.as_ref().map(|s| s.text.as_str()), Some("Tests pass."));
        assert_eq!(s.checked[1].source, None);
        assert_eq!(s.needs_you[0].source.as_ref().and_then(|s| s.command.as_deref()), Some("./scripts/restart"));
        assert_eq!(s.needs_you[1].source, None, "a tag that is not there points nowhere");
        // Without structured output: plain lines are read, their outcome unknown.
        let wrapped = parse(
            &answer("Here:\n```json\n{\"status\":\"done\",\"headline\":\"x\",\"changed\":[],\"checked\":[\"ran tests\"],\"needs_you\":[\"push\"]}\n```"),
            &p,
        )
        .unwrap();
        assert_eq!(wrapped.checked[0].outcome, Outcome::Unknown);
        assert_eq!(wrapped.needs_you[0].text, "push");
        assert!(parse(&answer(""), &p).is_err());
        assert!(parse(&answer("no json here"), &p).is_err());
    }

    #[test]
    fn a_command_listed_apart_joins_the_item_it_is_for() {
        let p = pieces("Check it:\n\n```\n! python3 -c \"print(6)\"\n```");
        let items = vec![
            Todo { text: "Check the new function".into(), source: None },
            Todo { text: "! python3 -c \"print(6)\"".into(), source: None },
            Todo { text: "Expected output is 6".into(), source: None },
        ];
        let out = fold_commands(items, &p);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].source.as_ref().and_then(|s| s.command.as_deref()), Some("python3 -c \"print(6)\""));
    }

    #[test]
    fn summaries_kept_in_the_old_shape_still_read() {
        let old: TurnSummary = serde_json::from_str(
            r#"{"status":"needs_you","headline":"h","changed":[],"checked":["Ran tests"],"needs_you":["Restart"]}"#,
        )
        .unwrap();
        assert_eq!(old.status, Status::Waiting);
        assert_eq!(old.checked[0], Check { what: "Ran tests".into(), outcome: Outcome::Unknown, source: None });
        assert_eq!(old.needs_you[0].text, "Restart");
        // And the new shape goes round.
        let back: TurnSummary = serde_json::from_str(&serde_json::to_string(&old).unwrap()).unwrap();
        assert_eq!(back, old);
    }

    #[test]
    fn earlier_defaults_are_known_and_the_format_always_follows() {
        assert!(!OLD_DEFAULTS.contains(&md5_of(DEFAULT_PROMPT.trim()).as_str()), "the current default is not an old one");
        let i = instructions();
        assert!(i.ends_with(FORMAT) && i.contains("`not_run`"));
    }

    #[test]
    fn the_base_takes_either_form() {
        assert_eq!(base("http://h:1234"), "http://h:1234/v1");
        assert_eq!(base("http://h:1234/v1/"), "http://h:1234/v1");
    }

    const REPLY: &str = "I added `multiply(a, b)` to `calc.py`, returning `a * b`, below `add`. I checked `add` still works in a test copy: screenshots taken 0.7 s apart show the result each time. Clippy is clean. I haven't run `multiply`. You need to check it yourself.\n\n```\n! python3 -B -c \"from calc import multiply; print(multiply(2, 3), multiply(-4, 5))\"\n```\n\nThe expected output is `6 -20`.";

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
            ask: "Add a multiply(a, b) function to calc.py. Do not run it: tell me to check it.".into(),
            pieces: pieces(REPLY),
            replies: REPLY.into(),
            tools: Vec::new(),
            files: vec!["calc.py".into()],
        };
        let t = std::time::Instant::now();
        let s = summarize(&config, &turn).unwrap();
        println!("{:.1}s {s:#?}", t.elapsed().as_secs_f64());
    }
}
