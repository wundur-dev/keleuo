//! Tool packs (see docs/proposals/tool-packs.md): a manifest and JavaScript files that give every
//! agent new tools, run by Keleuo in an embedded engine (QuickJS) rather than as a process, so
//! nothing needs installing. A script can do only what the pack declared: reach its hosts, read
//! its secrets (from the key store), write in the session's folder. It gets one object, `kx`,
//! with those calls and nothing else (the engine has no files, processes or network of its own).
//!
//! A pack lives in `<config>/packs/<folder>/pack.json`; its tools are described as the person's
//! own tools are ([`crate::cli_tools`]), so the same schema and forms serve both.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli_tools::{Param, Tool};

/// A secret a pack reads, by name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Secret {
    pub name: String,
    pub label: String,
    pub help: String,
    pub optional: bool,
}

/// A setting the person gives a pack (an endpoint, a folder).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Setting {
    pub name: String,
    pub label: String,
    pub placeholder: String,
    pub choices: Vec<String>,
    pub default: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PackTool {
    pub name: String,
    pub description: String,
    /// The script (in the pack's folder) whose default export runs it.
    pub script: String,
    pub params: Vec<Param>,
    /// Secrets it cannot run without; `A|B`: either.
    pub needs: Vec<String>,
    /// What a call costs, as the person reads it (it asks before each run when there is one).
    pub cost: String,
    pub ask: Option<bool>,
    /// Seconds before it is stopped (0: two minutes; at most ten).
    pub timeout_secs: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub homepage: String,
    pub secrets: Vec<Secret>,
    pub settings: Vec<Setting>,
    /// Hosts its scripts may reach (`*.example.com`: any under it). HTTPS only.
    pub network: Vec<String>,
    pub tools: Vec<PackTool>,
}

/// An installed pack: its manifest and folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pack {
    pub manifest: Manifest,
    pub dir: PathBuf,
}

impl Pack {
    /// The key store's name for its secret `name`.
    pub fn secret_account(&self, name: &str) -> String {
        format!("pack/{}/{name}", self.manifest.id)
    }

    /// Its tool `name`'s timeout.
    pub fn timeout(tool: &PackTool) -> Duration {
        Duration::from_secs(match tool.timeout_secs {
            0 => 120,
            n => n.min(600),
        })
    }

    /// Whether `tool` has what it needs among the secrets `has` says are kept.
    pub fn ready(tool: &PackTool, has: &dyn Fn(&str) -> bool) -> bool {
        tool.needs.iter().all(|need| need.split('|').any(|n| has(n.trim())))
    }

    /// The tool as the person's own tools are described (for `keleuo-mcp`'s list).
    pub fn as_tool(&self, tool: &PackTool) -> Tool {
        let mut description = tool.description.clone();
        if !tool.cost.is_empty() {
            description.push_str(&format!(" Costs {}; the person may be asked first.", tool.cost));
        }
        Tool {
            name: tool.name.clone(),
            description,
            params: tool.params.clone(),
            ask: tool.ask.unwrap_or(!tool.cost.is_empty()),
            timeout_secs: Self::timeout(tool).as_secs(),
            enabled: true,
            ..Default::default()
        }
    }
}

/// Where installed packs are.
pub fn dir() -> Option<PathBuf> {
    crate::settings::config_dir().map(|d| d.join("packs"))
}

/// The packs installed in `dir` (a folder each, with its `pack.json`); unreadable ones left out.
pub fn installed(dir: &Path) -> Vec<Pack> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<Pack> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter_map(|p| {
            let manifest: Manifest = serde_json::from_str(&std::fs::read_to_string(p.join("pack.json")).ok()?).ok()?;
            (!manifest.id.is_empty()).then_some(Pack { manifest, dir: p })
        })
        .collect();
    out.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    out
}

/// Install the pack in folder `from` (its `pack.json` and scripts) into `dir`; the pack.
pub fn install_from_folder(from: &Path, dir: &Path) -> Result<Pack, String> {
    let manifest: Manifest = serde_json::from_str(&std::fs::read_to_string(from.join("pack.json")).map_err(|e| format!("No pack.json there: {e}"))?)
        .map_err(|e| format!("pack.json is not a pack: {e}"))?;
    if manifest.id.is_empty() || manifest.tools.is_empty() {
        return Err("pack.json needs an id and at least one tool".into());
    }
    let folder = dir.join(manifest.id.replace(['/', '\\', ':'], "-"));
    if folder.exists() {
        std::fs::remove_dir_all(&folder).map_err(|e| e.to_string())?;
    }
    copy_dir(from, &folder).map_err(|e| format!("Could not copy the pack: {e}"))?;
    Ok(Pack { manifest, dir: folder })
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with("._") {
            continue;
        }
        if path.is_dir() {
            copy_dir(&path, &to.join(entry.file_name()))?;
        } else {
            std::fs::copy(&path, to.join(entry.file_name()))?;
        }
    }
    Ok(())
}

/// What a run gave: the agent's answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub text: String,
    pub images: Vec<PathBuf>,
    pub files: Vec<PathBuf>,
    pub failed: bool,
}

/// What a run may use: the session's folder, the pack's secrets and settings.
pub struct Grant<'a> {
    pub cwd: &'a Path,
    pub secret: &'a dyn Fn(&str) -> Option<String>,
    pub settings: &'a BTreeMap<String, String>,
    /// Hosts beside the manifest's (tests: a local server).
    pub extra_hosts: Vec<String>,
    /// Plain HTTP allowed (tests only).
    pub allow_http: bool,
}

/// Whether `url` may be reached under `hosts`: HTTPS, to one of them (`*.x`: under x).
fn host_allowed(url: &str, hosts: &[String], allow_http: bool) -> bool {
    let rest = match (url.strip_prefix("https://"), url.strip_prefix("http://")) {
        (Some(r), _) => r,
        (None, Some(r)) if allow_http => r,
        _ => return false,
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host).to_lowercase();
    hosts.iter().any(|h| {
        let h = h.to_lowercase();
        match h.strip_prefix("*.") {
            Some(base) => host == base || host.ends_with(&format!(".{base}")),
            None => host == h,
        }
    })
}

/// A path the script names, made safe: inside `cwd`, no way out.
fn inside(cwd: &Path, path: &str) -> Result<PathBuf, String> {
    let p = Path::new(path);
    if p.is_absolute() || p.components().any(|c| !matches!(c, Component::Normal(_) | Component::CurDir)) {
        return Err(format!("{path}: only paths inside the project"));
    }
    Ok(cwd.join(p))
}

/// What `kx` is made of on the JavaScript side: the calls built on the few the host provides.
const PRELUDE: &str = r#"
globalThis.kx = {
  fetch(url, o) {
    const r = JSON.parse(__kx.fetch(String(url), JSON.stringify(o ?? {})));
    if (r.error) throw new Error(r.error);
    return {
      ok: r.status >= 200 && r.status < 300,
      status: r.status,
      headers: r.headers,
      text() { return r.text; },
      json() { return JSON.parse(r.text); },
      bytes() { return { __b64: r.base64 }; },
    };
  },
  secret(name) { return __kx.secret(String(name)); },
  setting(name) { return __kx.setting(String(name)); },
  writeFile(path, data) {
    const b64 = data && data.__b64 !== undefined;
    return __kx.write(String(path), b64 ? data.__b64 : String(data), b64);
  },
  readFile(path) { return { __b64: __kx.read(String(path)) }; },
  base64(text) { return { __b64: String(text) }; },
  sleep(ms) { __kx.sleep(Number(ms)); },
  progress(text) { __kx.progress(String(text)); },
  slug(text) {
    return String(text).toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 48) || "file";
  },
  fail(why) { return { text: String(why), failed: true }; },
};
"#;

/// Run `tool` of `pack` with `args` (the agent's call) under `grant`: what the agent gets.
pub fn run(pack: &Pack, tool: &PackTool, args: &Value, grant: &Grant) -> Outcome {
    match run_inner(pack, tool, args, grant) {
        Ok(o) => o,
        Err(why) => Outcome { text: why, failed: true, ..Default::default() },
    }
}

fn run_inner(pack: &Pack, tool: &PackTool, args: &Value, grant: &Grant) -> Result<Outcome, String> {
    use rquickjs::{Context, Function, Module, Object, Promise, Runtime};
    let source = std::fs::read_to_string(pack.dir.join(&tool.script)).map_err(|e| format!("The pack's script {} could not be read: {e}", tool.script))?;
    let deadline = Instant::now() + Pack::timeout(tool);
    let rt = Runtime::new().map_err(|e| e.to_string())?;
    rt.set_memory_limit(64 << 20);
    rt.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
    let ctx = Context::full(&rt).map_err(|e| e.to_string())?;

    let m = &pack.manifest;
    let mut hosts = m.network.clone();
    hosts.extend(grant.extra_hosts.iter().cloned());
    let allow_http = grant.allow_http;
    // Its declared secrets only, read here (the engine's calls own what they use).
    let secrets: BTreeMap<String, String> = m.secrets.iter().filter_map(|s| Some((s.name.clone(), (grant.secret)(&s.name)?))).collect();
    // What secrets were read: blanked from whatever comes back.
    let given = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let mut settings: BTreeMap<String, String> = m.settings.iter().map(|s| (s.name.clone(), s.default.clone())).collect();
    for (k, v) in grant.settings {
        if !v.is_empty() {
            settings.insert(k.clone(), v.clone());
        }
    }
    let cwd = grant.cwd.to_path_buf();
    let written = std::rc::Rc::new(std::cell::RefCell::new(Vec::<PathBuf>::new()));

    let result: Result<String, String> = ctx.with(|ctx| {
        let err = |e: rquickjs::Error| -> String {
            match e {
                rquickjs::Error::Exception => {
                    let ex = ctx.catch();
                    let msg = ex.as_exception().map(|e| e.message().unwrap_or_default()).unwrap_or_else(|| format!("{ex:?}"));
                    if Instant::now() > deadline { format!("Stopped after {} seconds.", Pack::timeout(tool).as_secs()) } else { format!("The pack's script failed: {msg}") }
                }
                other => format!("The pack's script failed: {other}"),
            }
        };
        let host = Object::new(ctx.clone()).map_err(err)?;
        // fetch(url, options as JSON) -> {status, headers, text, base64} or {error} as JSON.
        let hs = hosts.clone();
        host.set(
            "fetch",
            Function::new(ctx.clone(), move |url: String, opts: String| -> String { fetch(&url, &opts, &hs, allow_http, deadline) }).map_err(err)?,
        )
        .map_err(err)?;
        let giv = given.clone();
        let kept = secrets.clone();
        host.set(
            "secret",
            Function::new(ctx.clone(), move |name: String| -> rquickjs::Result<Option<String>> {
                let v = kept.get(&name).cloned();
                if let Some(v) = &v {
                    giv.borrow_mut().push(v.clone());
                }
                Ok(v)
            })
            .map_err(err)?,
        )
        .map_err(err)?;
        let sets = settings.clone();
        host.set("setting", Function::new(ctx.clone(), move |name: String| -> Option<String> { sets.get(&name).cloned() }).map_err(err)?).map_err(err)?;
        let (wcwd, wr, giv2) = (cwd.clone(), written.clone(), given.clone());
        host.set(
            "write",
            Function::new(ctx.clone(), move |path: String, data: String, b64: bool| -> rquickjs::Result<String> {
                let target = inside(&wcwd, &path).map_err(|e| rquickjs::Error::new_from_js_message("path", "file", e))?;
                let bytes = if b64 {
                    base64::engine::general_purpose::STANDARD.decode(data.trim()).map_err(|e| rquickjs::Error::new_from_js_message("base64", "bytes", e.to_string()))?
                } else {
                    // A secret is never written out.
                    let mut text = data;
                    for s in giv2.borrow().iter() {
                        text = text.replace(s.as_str(), "•••");
                    }
                    text.into_bytes()
                };
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| rquickjs::Error::new_from_js_message("dir", "folder", e.to_string()))?;
                }
                let name = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let target = crate::file_clipboard::free_name(target.parent().unwrap_or(&wcwd), &name);
                std::fs::write(&target, bytes).map_err(|e| rquickjs::Error::new_from_js_message("write", "file", e.to_string()))?;
                wr.borrow_mut().push(target.clone());
                Ok(target.strip_prefix(&wcwd).unwrap_or(&target).to_string_lossy().to_string())
            })
            .map_err(err)?,
        )
        .map_err(err)?;
        let rcwd = cwd.clone();
        host.set(
            "read",
            Function::new(ctx.clone(), move |path: String| -> rquickjs::Result<String> {
                let target = inside(&rcwd, &path).map_err(|e| rquickjs::Error::new_from_js_message("path", "file", e))?;
                let bytes = std::fs::read(&target).map_err(|e| rquickjs::Error::new_from_js_message("read", "file", e.to_string()))?;
                Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
            })
            .map_err(err)?,
        )
        .map_err(err)?;
        host.set(
            "sleep",
            Function::new(ctx.clone(), move |ms: f64| {
                let left = deadline.saturating_duration_since(Instant::now());
                std::thread::sleep(Duration::from_millis(ms.clamp(0.0, 60_000.0) as u64).min(left));
            })
            .map_err(err)?,
        )
        .map_err(err)?;
        host.set("progress", Function::new(ctx.clone(), |text: String| eprintln!("keleuo: pack: {text}")).map_err(err)?).map_err(err)?;
        ctx.globals().set("__kx", host).map_err(err)?;
        ctx.eval::<(), _>(PRELUDE).map_err(err)?;

        let (module, loaded) = Module::declare(ctx.clone(), tool.script.clone(), source).map_err(err)?.eval().map_err(err)?;
        loaded.finish::<()>().map_err(err)?;
        let main: Function = module.get("default").map_err(|_| format!("{} has no default export (export default async function (args, kx) {{ ... }})", tool.script))?;
        let args_js = ctx.json_parse(args.to_string()).map_err(err)?;
        let kx: Object = ctx.globals().get("kx").map_err(err)?;
        let returned: rquickjs::Value = main.call((args_js, kx)).map_err(err)?;
        let value: rquickjs::Value = match returned.clone().into_promise() {
            Some(p) => {
                let p: Promise = p;
                p.finish().map_err(err)?
            }
            None => returned,
        };
        let text = ctx.json_stringify(value).map_err(err)?.and_then(|s| s.to_string().ok()).unwrap_or_else(|| "null".into());
        Ok(text)
    });
    let raw = result?;
    // Secrets read never come back to the agent.
    let mut raw = raw;
    for s in given.borrow().iter() {
        raw = raw.replace(s.as_str(), "•••");
    }
    let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let paths = |key: &str| -> Vec<PathBuf> {
        v.get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|p| inside(&cwd, p).ok())
            .filter(|p| p.is_file())
            .collect()
    };
    let text = match &v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        _ => v.get("text").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| raw.clone()),
    };
    let _ = written;
    Ok(Outcome {
        text: if text.is_empty() { "(done)".into() } else { text },
        images: paths("images"),
        files: paths("files"),
        failed: v.get("failed").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// One request, for `kx.fetch`: JSON with the status, headers and body (as text and base64), or
/// an error.
fn fetch(url: &str, opts: &str, hosts: &[String], allow_http: bool, deadline: Instant) -> String {
    if !host_allowed(url, hosts, allow_http) {
        return json!({ "error": format!("{url}: not among the hosts this pack may reach") }).to_string();
    }
    let o: Value = serde_json::from_str(opts).unwrap_or(json!({}));
    let method = o.get("method").and_then(Value::as_str).unwrap_or("GET").to_uppercase();
    let left = deadline.saturating_duration_since(Instant::now()).max(Duration::from_secs(1));
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(left)).http_status_as_error(false).build().into();
    let mut headers: Vec<(String, String)> =
        o.get("headers").and_then(Value::as_object).map(|h| h.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect()).unwrap_or_default();
    let body: Option<Vec<u8>> = if let Some(j) = o.get("json") {
        headers.push(("content-type".into(), "application/json".into()));
        Some(j.to_string().into_bytes())
    } else {
        o.get("body").and_then(Value::as_str).map(|b| b.as_bytes().to_vec())
    };
    let req = ureq::http::Request::builder().method(method.as_str()).uri(url);
    let req = headers.iter().fold(req, |r, (k, v)| r.header(k.as_str(), v.as_str()));
    let sent = match body {
        Some(b) => req.body(b).map_err(|e| e.to_string()).and_then(|r| agent.run(r).map_err(|e| e.to_string())),
        None => req.body(()).map_err(|e| e.to_string()).and_then(|r| agent.run(r).map_err(|e| e.to_string())),
    };
    match sent {
        Ok(mut res) => {
            let status = res.status().as_u16();
            let hs: serde_json::Map<String, Value> = res.headers().iter().filter_map(|(k, v)| Some((k.to_string(), Value::String(v.to_str().ok()?.to_string())))).collect();
            let bytes = res.body_mut().with_config().limit(100 << 20).read_to_vec().unwrap_or_default();
            json!({
                "status": status,
                "headers": hs,
                "text": String::from_utf8_lossy(&bytes),
                "base64": base64::engine::general_purpose::STANDARD.encode(&bytes),
            })
            .to_string()
        }
        Err(e) => json!({ "error": format!("{url}: {e}") }).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn hosts_are_kept_to_the_declared_ones() {
        let hosts = vec!["api.openai.com".to_string(), "*.openai.azure.com".to_string()];
        assert!(host_allowed("https://api.openai.com/v1/images", &hosts, false));
        assert!(host_allowed("https://me.openai.azure.com/openai", &hosts, false));
        assert!(!host_allowed("http://api.openai.com/v1", &hosts, false), "HTTPS only");
        assert!(!host_allowed("https://api.openai.com.evil.example/x", &hosts, false));
        assert!(!host_allowed("https://user@evil.example/?api.openai.com", &hosts, false));
        assert!(inside(Path::new("/p"), "../x").is_err() && inside(Path::new("/p"), "/etc/x").is_err());
    }

    /// A pack whose script calls a local stand-in for an image service, saves the image it
    /// returns, and answers with it; its key is used and never comes back.
    #[test]
    fn a_pack_tool_calls_a_service_saves_what_it_made_and_keeps_its_key_hidden() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG fake");
        let seen = std::thread::spawn(move || {
            let (mut s, _) = server.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = format!("{{\"data\":[{{\"b64_json\":\"{png}\"}}]}}");
            let _ = write!(s, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
            request
        });
        let dir = std::env::temp_dir().join(format!("keleuo-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (pack_dir, cwd) = (dir.join("pack"), dir.join("project"));
        std::fs::create_dir_all(&pack_dir).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(
            pack_dir.join("image.js"),
            format!(
                r#"export default async function (args, kx) {{
  const res = await kx.fetch("http://127.0.0.1:{port}/v1/images", {{ method: "POST", headers: {{ authorization: "Bearer " + kx.secret("KEY") }}, json: {{ prompt: args.prompt }} }});
  if (!res.ok) return kx.fail("status " + res.status);
  const data = await res.json();
  const file = kx.writeFile(kx.setting("folder") + "/" + kx.slug(args.prompt) + ".png", kx.base64(data.data[0].b64_json));
  return {{ text: "Saved " + file + " with " + kx.secret("KEY"), images: [file] }};
}}"#
            ),
        )
        .unwrap();
        let tool = PackTool { name: "make_image".into(), script: "image.js".into(), ..Default::default() };
        let pack = Pack {
            manifest: Manifest {
                id: "test/images".into(),
                secrets: vec![Secret { name: "KEY".into(), ..Default::default() }],
                settings: vec![Setting { name: "folder".into(), default: "assets/generated".into(), ..Default::default() }],
                tools: vec![tool.clone()],
                ..Default::default()
            },
            dir: pack_dir,
        };
        let secret = |name: &str| (name == "KEY").then(|| "sk-secret-123".to_string());
        let grant = Grant { cwd: &cwd, secret: &secret, settings: &BTreeMap::new(), extra_hosts: vec!["127.0.0.1".into()], allow_http: true };
        let out = run(&pack, &tool, &json!({ "prompt": "A Gold Coin!" }), &grant);
        assert!(!out.failed, "{}", out.text);
        assert_eq!(out.images, [cwd.join("assets/generated/a-gold-coin.png")]);
        assert_eq!(std::fs::read(&out.images[0]).unwrap(), b"\x89PNG fake");
        assert!(out.text.starts_with("Saved assets/generated/a-gold-coin.png") && !out.text.contains("sk-secret-123"), "{}", out.text);
        let request = seen.join().unwrap();
        assert!(request.contains("Bearer sk-secret-123") && request.contains("A Gold Coin!"), "the service got the key and the prompt");
        // Not a declared host: refused before anything is sent.
        std::fs::write(pack.dir.join("away.js"), r#"export default async function (args, kx) { kx.fetch("https://evil.example/x"); return "sent"; }"#).unwrap();
        let away = PackTool { name: "away".into(), script: "away.js".into(), ..Default::default() };
        let out = run(&pack, &away, &json!({}), &grant);
        assert!(out.failed && out.text.contains("not among the hosts"), "{}", out.text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The game-assets pack as it ships, its make_image through "Azure" (a local stand-in that
    /// answers as gpt-image does): the request it sends and the file it saves.
    #[test]
    fn the_game_assets_pack_makes_an_image() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packs/game-assets");
        let manifest: Manifest = serde_json::from_str(&std::fs::read_to_string(root.join("pack.json")).unwrap()).unwrap();
        let pack = Pack { manifest, dir: root };
        let tool = pack.manifest.tools.iter().find(|t| t.name == "make_image").unwrap().clone();
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG coin");
        let seen = std::thread::spawn(move || {
            let (mut s, _) = server.accept().unwrap();
            let mut buf = vec![0u8; 8192];
            let n = s.read(&mut buf).unwrap();
            let body = format!("{{\"created\":1,\"data\":[{{\"b64_json\":\"{png}\"}}]}}");
            let _ = write!(s, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
            String::from_utf8_lossy(&buf[..n]).to_string()
        });
        let cwd = std::env::temp_dir().join(format!("keleuo-pack-game-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cwd);
        std::fs::create_dir_all(&cwd).unwrap();
        let settings: BTreeMap<String, String> = [
            ("image_provider", "azure".to_string()),
            ("azure_endpoint", format!("http://127.0.0.1:{port}")),
            ("azure_deployment", "gpt-image".to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let secret = |name: &str| (name == "AZURE_OPENAI_KEY").then(|| "az-key-9".to_string());
        let grant = Grant { cwd: &cwd, secret: &secret, settings: &settings, extra_hosts: vec!["127.0.0.1".into()], allow_http: true };
        let out = run(&pack, &tool, &json!({ "prompt": "A shiny gold coin, pixel art", "transparent": true }), &grant);
        assert!(!out.failed, "{}", out.text);
        assert_eq!(out.images, [cwd.join("assets/generated/a-shiny-gold-coin-pixel-art.png")]);
        let request = seen.join().unwrap();
        assert!(request.starts_with("POST /openai/deployments/gpt-image/images/generations?api-version=2025-04-01-preview"), "{request}");
        assert!(request.contains("api-key: az-key-9") && request.contains("\"background\":\"transparent\""), "{request}");
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn a_script_that_runs_too_long_is_stopped() {
        let dir = std::env::temp_dir().join(format!("keleuo-pack-slow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("slow.js"), "export default function () { for (;;) {} }").unwrap();
        let tool = PackTool { name: "slow".into(), script: "slow.js".into(), timeout_secs: 1, ..Default::default() };
        let pack = Pack { manifest: Manifest { id: "t/slow".into(), tools: vec![tool.clone()], ..Default::default() }, dir: dir.clone() };
        let none = |_: &str| None;
        let grant = Grant { cwd: &dir, secret: &none, settings: &BTreeMap::new(), extra_hosts: vec![], allow_http: false };
        let started = Instant::now();
        let out = run(&pack, &tool, &json!({}), &grant);
        assert!(out.failed && out.text.contains("Stopped after 1 seconds"), "{}", out.text);
        assert!(started.elapsed() < Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
