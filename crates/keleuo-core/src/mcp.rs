//! The tool servers (MCP) Keleuo gives every agent it starts, set up once in Settings: for the
//! whole app, or for one folder (servers of its own, and app ones turned off there). Each agent
//! gets them its own way: Claude Code a config file (`--mcp-config`), Cursor its session
//! request (ACP), Codex its config overrides. Nothing in the agents' own settings changes.
//!
//! Secret values (an API key, a token, a database URL) are kept in the system's key store
//! ([`crate::secrets`]), never in a file: a server's config names them as `${VAR}`, and the
//! agent is started with those variables set from the store.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// One tool server.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Server {
    pub name: String,
    /// A local server: the command that starts it, and its arguments...
    pub command: String,
    pub args: Vec<String>,
    /// ...or a remote one, by its address.
    pub url: String,
    /// What it is given: environment variables (a local server) or headers (a remote one),
    /// their values with `${NAME}` for a secret kept as `NAME` (see [`Server::secrets`]).
    pub env: BTreeMap<String, String>,
    pub headers: BTreeMap<String, String>,
    /// The secrets it uses, by name, kept in the key store (see [`secret_account`]).
    pub secrets: Vec<String>,
    /// Folders it writes in, beside the session's (a local server runs inside the agent's
    /// write sandbox).
    pub writes: Vec<PathBuf>,
    pub enabled: bool,
}

impl Server {
    pub fn is_remote(&self) -> bool {
        !self.url.trim().is_empty()
    }
}

/// A folder's own: servers only it has (or that replace an app one of the same name), and app
/// servers turned off in it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FolderServers {
    pub servers: Vec<Server>,
    pub off: Vec<String>,
}

/// The servers an agent working in `folder` gets: the app's that are on and not turned off
/// for the folder, and the folder's own that are on (one of the same name replaces the app's).
/// A folder's settings apply to the folders inside it too: the nearest one with any counts.
pub fn servers_for(app: &[Server], folders: &BTreeMap<PathBuf, FolderServers>, folder: &Path) -> Vec<Server> {
    let own = folder.ancestors().find_map(|f| folders.get(f));
    let mut out: Vec<Server> = app
        .iter()
        .filter(|s| s.enabled)
        .filter(|s| !own.is_some_and(|o| o.off.contains(&s.name) || o.servers.iter().any(|f| f.name == s.name)))
        .cloned()
        .collect();
    if let Some(own) = own {
        out.extend(own.servers.iter().filter(|s| s.enabled).cloned());
    }
    out
}

/// The key store's name for secret `name` of server `server`.
pub fn secret_account(server: &str, name: &str) -> String {
    format!("mcp/{server}/{name}")
}

/// The variable an agent is started with for secret `name` of server `server`: one of its own
/// per server, so two servers' `API_KEY`s do not meet.
pub fn secret_var(server: &str, name: &str) -> String {
    let clean = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }).collect::<String>();
    format!("KELEUO_MCP_{}_{}", clean(server), clean(name))
}

/// `value` with each `${NAME}` of a secret of `server` as its per-server variable.
fn with_vars(server: &Server, value: &str) -> String {
    let mut v = value.to_string();
    for name in &server.secrets {
        v = v.replace(&format!("${{{name}}}"), &format!("${{{}}}", secret_var(&server.name, name)));
    }
    v
}

/// Claude Code's tool server file (`--mcp-config`): `servers`, plus `extra` entries (Keleuo's
/// own) as they are. Secrets stay `${VAR}`s, which Claude Code fills in from its environment.
pub fn claude_config(servers: &[Server], extra: &serde_json::Map<String, Value>) -> Value {
    let mut all = extra.clone();
    for s in servers {
        let entry = if s.is_remote() {
            let headers: serde_json::Map<String, Value> = s.headers.iter().map(|(k, v)| (k.clone(), Value::String(with_vars(s, v)))).collect();
            json!({ "type": "http", "url": with_vars(s, &s.url), "headers": headers })
        } else {
            let env: serde_json::Map<String, Value> = s.env.iter().map(|(k, v)| (k.clone(), Value::String(with_vars(s, v)))).collect();
            json!({ "type": "stdio", "command": s.command, "args": s.args, "env": env })
        };
        all.insert(s.name.clone(), entry);
    }
    json!({ "mcpServers": all })
}

/// The servers in a Claude Code tool server file (as [`claude_config`] writes), as ACP's
/// `session/new` takes them: local ones with their command and environment, remote ones with
/// their address and headers, each `${VAR}` filled in from `env` (the values go to the agent
/// over its pipe, never to a file).
pub fn acp_servers(config: &Value, env: &[(String, String)]) -> Value {
    let fill = |s: &str| {
        let mut out = s.to_string();
        for (k, v) in env {
            out = out.replace(&format!("${{{k}}}"), v);
        }
        out
    };
    let pairs = |m: Option<&Value>| -> Vec<Value> {
        m.and_then(Value::as_object)
            .map(|m| m.iter().map(|(k, v)| json!({ "name": k, "value": fill(v.as_str().unwrap_or_default()) })).collect())
            .unwrap_or_default()
    };
    let servers = config.get("mcpServers").and_then(Value::as_object).cloned().unwrap_or_default();
    Value::Array(
        servers
            .iter()
            .map(|(name, s)| {
                let text = |k: &str| s.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                if s.get("type").and_then(Value::as_str) == Some("http") {
                    json!({ "type": "http", "name": name, "url": fill(&text("url")), "headers": pairs(s.get("headers")) })
                } else {
                    let args: Vec<String> = s.get("args").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(fill)).collect()).unwrap_or_default();
                    json!({ "name": name, "command": text("command"), "args": args, "env": pairs(s.get("env")) })
                }
            })
            .collect(),
    )
}

/// The variables the agent is started with: each secret `servers` use, read from the key
/// store. A secret not there is left out (its server then gets the `${VAR}` unfilled, and says
/// so).
pub fn secret_env(servers: &[Server]) -> Vec<(String, String)> {
    servers
        .iter()
        .flat_map(|s| s.secrets.iter().map(move |name| (s, name)))
        .filter_map(|(s, name)| crate::secrets::get(&secret_account(&s.name, name)).map(|v| (secret_var(&s.name, name), v)))
        .collect()
}

/// Folders `servers` write in (for the agent's write sandbox).
pub fn writes(servers: &[Server]) -> Vec<PathBuf> {
    servers.iter().filter(|s| !s.is_remote()).flat_map(|s| s.writes.iter().cloned()).collect()
}

/// A server found in Claude Code's own settings, to import: the server, with its environment
/// and headers turned into secrets (`${NAME}`), and their values to keep in the key store.
pub struct Found {
    pub server: Server,
    pub values: Vec<(String, String)>,
}

/// The servers in Claude Code's settings (`~/.claude.json`): its own (user) ones, and those
/// for `folder` when given.
pub fn found_in_claude(home: &Path, folder: Option<&Path>) -> Vec<Found> {
    let Ok(text) = std::fs::read_to_string(home.join(".claude.json")) else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return Vec::new() };
    let mut out = from_claude(v.get("mcpServers"));
    if let Some(folder) = folder {
        out.extend(from_claude(v.get("projects").and_then(|p| p.get(folder.to_string_lossy().as_ref())).and_then(|p| p.get("mcpServers"))));
    }
    out
}

/// Servers from a Claude Code `mcpServers` object. Every environment value and header becomes
/// a secret: Keleuo cannot tell a key from a setting, and keeping both safe costs nothing.
fn from_claude(servers: Option<&Value>) -> Vec<Found> {
    let Some(map) = servers.and_then(Value::as_object) else { return Vec::new() };
    let strings = |v: Option<&Value>| -> Vec<(String, String)> {
        v.and_then(Value::as_object)
            .map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect())
            .unwrap_or_default()
    };
    map.iter()
        .map(|(name, s)| {
            let text = |k: &str| s.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
            let env = strings(s.get("env"));
            let headers = strings(s.get("headers"));
            let mut values = Vec::new();
            let mut secret = |key: &str, value: String| {
                let n = key.to_ascii_uppercase().replace('-', "_");
                values.push((n.clone(), value));
                format!("${{{n}}}")
            };
            let env: BTreeMap<String, String> = env.into_iter().map(|(k, v)| (k.clone(), secret(&k, v))).collect();
            let headers: BTreeMap<String, String> = headers.into_iter().map(|(k, v)| (k.clone(), secret(&k, v))).collect();
            let server = Server {
                name: name.clone(),
                command: text("command"),
                args: s.get("args").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
                url: text("url"),
                env,
                headers,
                secrets: values.iter().map(|(n, _)| n.clone()).collect(),
                writes: Vec::new(),
                enabled: true,
            };
            Found { server, values }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(name: &str) -> Server {
        Server { name: name.into(), command: "run".into(), enabled: true, ..Default::default() }
    }

    #[test]
    fn a_folder_turns_app_servers_off_adds_its_own_and_reaches_folders_inside() {
        let app = vec![server("github"), server("playwright"), Server { enabled: false, ..server("old") }];
        let mut folders = BTreeMap::new();
        folders.insert(PathBuf::from("/w/app"), FolderServers { servers: vec![server("postgres")], off: vec!["playwright".into()] });
        let names = |f: &str| servers_for(&app, &folders, Path::new(f)).into_iter().map(|s| s.name).collect::<Vec<_>>();
        assert_eq!(names("/w/app"), ["github", "postgres"]);
        assert_eq!(names("/w/app/crates/x"), ["github", "postgres"], "inside the folder too");
        assert_eq!(names("/w/other"), ["github", "playwright"]);
    }

    #[test]
    fn secrets_stay_variables_in_claude_s_file() {
        let s = Server {
            name: "pg-dev".into(),
            command: "uvx".into(),
            args: vec!["mcp-server-postgres".into()],
            env: [("DATABASE_URL".to_string(), "${DATABASE_URL}".to_string())].into(),
            secrets: vec!["DATABASE_URL".into()],
            enabled: true,
            ..Default::default()
        };
        let config = claude_config(&[s], &serde_json::Map::new());
        assert_eq!(config["mcpServers"]["pg-dev"]["env"]["DATABASE_URL"], "${KELEUO_MCP_PG_DEV_DATABASE_URL}");
        assert_eq!(config["mcpServers"]["pg-dev"]["type"], "stdio");
    }

    #[test]
    fn acp_gets_the_servers_with_their_secrets_filled_in() {
        let s = Server {
            name: "pg".into(),
            command: "uvx".into(),
            env: [("DATABASE_URL".to_string(), "${DATABASE_URL}".to_string())].into(),
            secrets: vec!["DATABASE_URL".into()],
            enabled: true,
            ..Default::default()
        };
        let config = claude_config(&[s], &serde_json::Map::new());
        let acp = acp_servers(&config, &[(secret_var("pg", "DATABASE_URL"), "postgres://x".into())]);
        assert_eq!(acp[0]["name"], "pg");
        assert_eq!(acp[0]["env"][0], json!({ "name": "DATABASE_URL", "value": "postgres://x" }));
    }

    #[test]
    fn claude_s_servers_import_with_their_values_as_secrets() {
        let v = json!({ "gh": { "type": "http", "url": "https://x/mcp", "headers": { "Authorization": "Bearer abc" } } });
        let found = from_claude(Some(&v));
        assert_eq!(found[0].server.headers["Authorization"], "${AUTHORIZATION}");
        assert_eq!(found[0].values, vec![("AUTHORIZATION".to_string(), "Bearer abc".to_string())]);
        assert!(found[0].server.is_remote());
    }
}
