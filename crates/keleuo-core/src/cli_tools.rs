//! Tools made from command-line programs: the person points Keleuo at a program (a script that
//! makes images with their own cloud credits, a deploy command), says what it takes, and every
//! agent gets it as a tool, beside Keleuo's own (through `keleuo-mcp`). The window runs it, with
//! the secrets it needs from the key store (the agent never sees them), in the session's folder,
//! its writes kept to that folder and the ones it names.
//!
//! The command is a list of words, each of which may hold placeholders: `{name}` for a
//! parameter's value, `{name?text}` for `text` when a yes/no parameter is yes (or any other is
//! given). A word that is only a placeholder of a parameter not given goes, with the flag
//! before it (`--size {size}`). Each word is one argument: no shell reads it, so a value is never
//! taken as more than itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The names Keleuo's own tools have; a tool may not take one.
pub const RESERVED: [&str; 3] = ["list_sessions", "send_message", "ask_for_review"];

/// What a parameter takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Text,
    Number,
    /// Yes or no.
    Flag,
    /// One of [`Param::choices`].
    Choice,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Param {
    pub name: String,
    pub kind: Kind,
    pub description: String,
    pub required: bool,
    pub choices: Vec<String>,
}

/// What the agent gets back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Output {
    /// What it printed.
    #[default]
    Text,
    /// What it printed, and the images whose paths it printed, as images.
    Images,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tool {
    pub name: String,
    /// What the agent reads to know when to use it.
    pub description: String,
    /// The program and its arguments, with placeholders.
    pub command: Vec<String>,
    pub params: Vec<Param>,
    /// Secrets it gets as environment variables, by name, from the key store.
    pub secrets: Vec<String>,
    /// Folders it writes in, beside the session's.
    pub writes: Vec<PathBuf>,
    pub output: Output,
    /// Seconds before it is stopped (0: two minutes).
    pub timeout_secs: u64,
    /// The person allows each run.
    pub ask: bool,
    pub enabled: bool,
}

impl Tool {
    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(if self.timeout_secs == 0 { 120 } else { self.timeout_secs })
    }
}

/// A folder's own tools, and app ones turned off in it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FolderTools {
    pub tools: Vec<Tool>,
    pub off: Vec<String>,
}

/// The tools an agent in `folder` gets: as [`crate::mcp::servers_for`] does for servers.
pub fn tools_for(app: &[Tool], folders: &BTreeMap<PathBuf, FolderTools>, folder: &Path) -> Vec<Tool> {
    let own = folder.ancestors().find_map(|f| folders.get(f));
    let mut out: Vec<Tool> = app
        .iter()
        .filter(|t| t.enabled)
        .filter(|t| !own.is_some_and(|o| o.off.contains(&t.name) || o.tools.iter().any(|f| f.name == t.name)))
        .cloned()
        .collect();
    if let Some(own) = own {
        out.extend(own.tools.iter().filter(|t| t.enabled).cloned());
    }
    out
}

/// The key store's name for secret `name` of tool `tool`.
pub fn secret_account(tool: &str, name: &str) -> String {
    format!("tool/{tool}/{name}")
}

/// The placeholders in `words`, in order, each once: the parameters a command takes.
pub fn placeholders(words: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for w in words {
        let mut rest = w.as_str();
        while let Some(start) = rest.find('{') {
            let Some(end) = rest[start..].find('}') else { break };
            let inner = &rest[start + 1..start + end];
            let name = inner.split('?').next().unwrap_or_default().trim().to_string();
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !out.contains(&name) {
                out.push(name);
            }
            rest = &rest[start + end + 1..];
        }
    }
    out
}

/// The tool as MCP describes it: its name, description and input schema.
pub fn describe(tool: &Tool) -> Value {
    let mut props = serde_json::Map::new();
    for p in &tool.params {
        let mut s = match p.kind {
            Kind::Text => json!({ "type": "string" }),
            Kind::Number => json!({ "type": "number" }),
            Kind::Flag => json!({ "type": "boolean" }),
            Kind::Choice => json!({ "type": "string", "enum": p.choices }),
        };
        if !p.description.is_empty() {
            s["description"] = Value::String(p.description.clone());
        }
        props.insert(p.name.clone(), s);
    }
    let required: Vec<&str> = tool.params.iter().filter(|p| p.required).map(|p| p.name.as_str()).collect();
    json!({
        "name": tool.name,
        "description": tool.description,
        "inputSchema": { "type": "object", "properties": props, "required": required },
    })
}

/// A parameter's value as the command gets it; `None` when not given (or a flag that is no).
fn value(p: &Param, args: &Value) -> Result<Option<String>, String> {
    let v = args.get(&p.name).filter(|v| !v.is_null());
    let text = match (p.kind, v) {
        (_, None) => None,
        (Kind::Flag, Some(v)) => v.as_bool().or_else(|| v.as_str().map(|s| s == "true")).filter(|b| *b).map(|_| "true".to_string()),
        (Kind::Number, Some(v)) => {
            let n = v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).ok_or(format!("{} must be a number", p.name))?;
            Some(if n.fract() == 0.0 && n.abs() < 1e15 { format!("{}", n as i64) } else { n.to_string() })
        }
        (_, Some(v)) => Some(v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())),
    };
    if let (Kind::Choice, Some(t)) = (p.kind, &text)
        && !p.choices.contains(t)
    {
        return Err(format!("{} must be one of: {}", p.name, p.choices.join(", ")));
    }
    if p.required && text.is_none() && p.kind != Kind::Flag {
        return Err(format!("{} is required", p.name));
    }
    Ok(text)
}

/// The command's arguments for a call with `args` (the program first), or why not.
pub fn argv(tool: &Tool, args: &Value) -> Result<Vec<String>, String> {
    let mut values: BTreeMap<String, Option<String>> = BTreeMap::new();
    for p in &tool.params {
        values.insert(p.name.clone(), value(p, args)?);
    }
    let mut out: Vec<String> = Vec::new();
    for word in &tool.command {
        // A word that is only one placeholder, of a parameter not given: it goes, and a flag
        // just before it goes too.
        if let Some(inner) = word.strip_prefix('{').and_then(|w| w.strip_suffix('}'))
            && !inner.contains(['{', '}'])
        {
            let name = inner.split('?').next().unwrap_or_default();
            if values.get(name).is_some_and(Option::is_none) {
                if !inner.contains('?') && out.len() > 1 && out.last().is_some_and(|w| w.starts_with('-')) {
                    out.pop();
                }
                continue;
            }
        }
        out.push(fill(word, &values));
    }
    if out.is_empty() || out[0].is_empty() {
        return Err("the tool has no command".into());
    }
    Ok(out)
}

/// `word` with its placeholders filled in.
fn fill(word: &str, values: &BTreeMap<String, Option<String>>) -> String {
    let mut out = String::new();
    let mut rest = word;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else { break };
        out.push_str(&rest[..start]);
        let inner = &rest[start + 1..start + end];
        let (name, text) = match inner.split_once('?') {
            Some((n, t)) => (n, Some(t)),
            None => (inner, None),
        };
        match (values.get(name), text) {
            (Some(Some(v)), None) => out.push_str(v),
            (Some(Some(_)), Some(t)) => out.push_str(t),
            (Some(None), _) => {}
            // Not a parameter: braces kept as they are.
            (None, _) => out.push_str(&rest[start..start + end + 1]),
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}

/// Images whose paths `output` names (one per line, or the last word of a line), relative ones
/// from `cwd`.
pub fn images_in(output: &str, cwd: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for line in output.lines() {
        for word in [line.trim(), line.split_whitespace().last().unwrap_or_default()] {
            let word = word.trim_matches(|c: char| c == '"' || c == '\'' || c == '`');
            let lower = word.to_lowercase();
            if [".png", ".jpg", ".jpeg", ".webp", ".gif"].iter().any(|e| lower.ends_with(e)) {
                let p = if Path::new(word).is_absolute() { PathBuf::from(word) } else { cwd.join(word) };
                if p.is_file() && !out.contains(&p) {
                    out.push(p);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> Tool {
        Tool {
            name: "make_image".into(),
            command: ["python3", "~/tools/img.py", "--prompt", "{prompt}", "--size", "{size}", "{hd?--hd}", "--out={out}"].map(String::from).to_vec(),
            params: vec![
                Param { name: "prompt".into(), required: true, ..Default::default() },
                Param { name: "size".into(), kind: Kind::Choice, choices: vec!["1024x1024".into(), "1792x1024".into()], ..Default::default() },
                Param { name: "hd".into(), kind: Kind::Flag, ..Default::default() },
                Param { name: "out".into(), ..Default::default() },
            ],
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn a_call_fills_one_argument_per_word_and_drops_what_was_not_given() {
        let a = argv(&tool(), &json!({ "prompt": "a cat; rm -rf ~", "hd": true, "out": "cat.png" })).unwrap();
        assert_eq!(a, ["python3", "~/tools/img.py", "--prompt", "a cat; rm -rf ~", "--hd", "--out=cat.png"]);
        let b = argv(&tool(), &json!({ "prompt": "x", "size": "1792x1024" })).unwrap();
        assert_eq!(b, ["python3", "~/tools/img.py", "--prompt", "x", "--size", "1792x1024", "--out="]);
    }

    #[test]
    fn calls_are_checked_against_the_parameters() {
        assert_eq!(argv(&tool(), &json!({})).unwrap_err(), "prompt is required");
        assert!(argv(&tool(), &json!({ "prompt": "x", "size": "big" })).unwrap_err().contains("one of"));
    }

    #[test]
    fn the_command_s_placeholders_are_its_parameters() {
        assert_eq!(placeholders(&tool().command), ["prompt", "size", "hd", "out"]);
        let d = describe(&tool());
        assert_eq!(d["inputSchema"]["required"], json!(["prompt"]));
        assert_eq!(d["inputSchema"]["properties"]["size"]["enum"], json!(["1024x1024", "1792x1024"]));
    }

    #[test]
    fn printed_image_paths_are_found() {
        let dir = std::env::temp_dir().join(format!("kl-tool-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cat.png"), b"png").unwrap();
        assert_eq!(images_in("Saved image to cat.png\nno.png\n", &dir), [dir.join("cat.png")]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
