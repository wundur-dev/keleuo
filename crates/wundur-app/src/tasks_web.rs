//! The tasks as a page in the person's browser, beside the pages they work through: the same
//! lists and cards as the Tasks panel, kept in step both ways. Ticking a card on the page ticks
//! it here (and in `tasks.json`); a change here (an agent's new list, a task it ticked, the
//! person ticking or removing) reaches the open page without a reload.
//!
//! A small HTTP server on its own thread, on 127.0.0.1 only. Every request needs the secret
//! token in its path (`/<token>/...`) and a `Host` of `127.0.0.1:<port>` or `localhost:<port>`,
//! so other machines, other sites in the browser and DNS rebinding cannot reach it. The page
//! hears of changes through server-sent events (`/<token>/events`: the whole state, on connect
//! and after each change) and sends ticks and removals as POSTs, which reach the UI thread
//! through a channel.
//!
//! It starts the first time the person opens the page, and at startup after that, on the port
//! and token it had before (kept in `tasks-web.json`), so a page left open picks up again after
//! a restart.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use gpui_kit::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The page: one file, its styles and script inline, nothing fetched from elsewhere.
const PAGE: &str = include_str!("tasks_web.html");
/// Connections served at once (each on its own thread); past this, new ones are turned away.
const MAX_CONNECTIONS: usize = 64;
/// The most a request's head or body may be.
const MAX_HEAD: usize = 16 * 1024;
const MAX_BODY: usize = 16 * 1024;
/// How often an idle event stream sends a comment, which also notices a page that went away.
const KEEPALIVE: Duration = Duration::from_secs(15);

/// The running server.
pub struct Server {
    port: u16,
    token: String,
    shared: Arc<Shared>,
}

impl Global for Server {}

/// What the server's threads share: the lists as the page reads them, numbered so each event
/// stream knows when there is something new.
struct Shared {
    state: Mutex<(u64, String)>,
    changed: Condvar,
    connections: AtomicUsize,
}

/// A change the page asks for, carried out on the UI thread.
#[derive(Debug, PartialEq)]
enum Action {
    Tick { id: u64, done: bool },
    Remove { id: u64 },
}

/// The port and token, kept so a restart serves the open page again.
#[derive(Serialize, Deserialize)]
struct Saved {
    port: u16,
    token: String,
}

fn saved_file() -> Option<PathBuf> {
    wundur_core::settings::config_dir().map(|d| d.join("tasks-web.json"))
}

/// At startup: back on, when the person has opened the page before.
pub fn init(cx: &mut App) {
    if saved_file().is_some_and(|f| f.exists()) {
        let _ = ensure(cx);
    }
}

/// The page in the person's browser (starting the server first, when it is not running).
pub fn open(cx: &mut App) {
    if let Some(url) = ensure(cx) {
        cx.open_url(&url);
    }
}

/// The page's address, starting the server when needed; `None` when it could not start.
pub fn ensure(cx: &mut App) -> Option<String> {
    if !cx.has_global::<Server>() {
        let server = start(cx).ok()?;
        cx.set_global(server);
        publish(cx);
    }
    let s = cx.global::<Server>();
    Some(format!("http://127.0.0.1:{}/{}/", s.port, s.token))
}

/// Tell the open pages the lists changed; nothing when the server is not running.
pub fn publish(cx: &mut App) {
    let Some(shared) = cx.try_global::<Server>().map(|s| s.shared.clone()) else {
        return;
    };
    let json = snapshot(&crate::tasks::lists(cx));
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    if state.1 != json {
        *state = (state.0 + 1, json);
        shared.changed.notify_all();
    }
}

/// The lists as the page reads them: each list's folder by its name only, as the panel shows it.
fn snapshot(lists: &[crate::tasks::TaskList]) -> String {
    let lists: Vec<Value> = lists
        .iter()
        .map(|l| {
            json!({
                "id": l.id,
                "title": l.title,
                "folder": l.folder.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                "from": l.from,
                "made": l.made,
                "tasks": l.tasks.iter().map(|t| json!({
                    "id": t.id, "title": t.title, "detail": t.detail, "command": t.command, "link": t.link, "done": t.done,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({ "lists": lists }).to_string()
}

/// Bind, then serve on a thread of its own; the page's changes come back over a channel.
fn start(cx: &mut App) -> io::Result<Server> {
    let saved = saved_file().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| serde_json::from_str::<Saved>(&t).ok());
    let (listener, port, token) = bind(saved)?;
    if let Some(f) = saved_file() {
        let _ = std::fs::write(f, serde_json::to_string(&Saved { port, token: token.clone() }).unwrap_or_default());
    }
    let shared = Arc::new(Shared { state: Mutex::new((0, String::new())), changed: Condvar::new(), connections: AtomicUsize::new(0) });
    let (tx, rx) = async_channel::unbounded::<Action>();
    {
        let (shared, token) = (shared.clone(), token.clone());
        std::thread::Builder::new().name("tasks-web".into()).spawn(move || accept(listener, port, token, shared, tx))?;
    }
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok(action) = rx.recv().await {
            cx.update(|cx| apply(action, cx));
        }
    })
    .detach();
    Ok(Server { port, token, shared })
}

/// A listener on 127.0.0.1: on the port and with the token from before when that port is free,
/// else on a new free port with a new token.
fn bind(saved: Option<Saved>) -> io::Result<(TcpListener, u16, String)> {
    if let Some(s) = saved.filter(|s| s.port != 0 && s.token.len() >= 32)
        && let Ok(l) = TcpListener::bind((Ipv4Addr::LOCALHOST, s.port))
    {
        return Ok((l, s.port, s.token));
    }
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = l.local_addr()?.port();
    Ok((l, port, new_token()?))
}

/// 32 random bytes, as hex.
fn new_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn apply(action: Action, cx: &mut App) {
    match action {
        Action::Tick { id, done } => {
            crate::tasks::set_done(id, done, cx);
        }
        Action::Remove { id } => crate::tasks::remove_list(id, cx),
    }
}

fn accept(listener: TcpListener, port: u16, token: String, shared: Arc<Shared>, tx: async_channel::Sender<Action>) {
    for stream in listener.incoming().flatten() {
        if shared.connections.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            shared.connections.fetch_sub(1, Ordering::SeqCst);
            let mut stream = stream;
            let _ = respond(&mut stream, "503 Service Unavailable", "text/plain", b"Busy");
            continue;
        }
        let (token, shared, tx) = (token.clone(), shared.clone(), tx.clone());
        std::thread::spawn(move || {
            serve(stream, port, &token, &shared, &tx);
            shared.connections.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

/// One connection: one request, then closed (an event stream stays until the page leaves).
fn serve(mut stream: TcpStream, port: u16, token: &str, shared: &Shared, tx: &async_channel::Sender<Action>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let Ok(reader) = stream.try_clone() else { return };
    let Ok(request) = read_request(&mut BufReader::new(reader)) else {
        let _ = respond(&mut stream, "400 Bad Request", "text/plain", b"Bad request");
        return;
    };
    let _ = match route(&request, port, token) {
        Route::Page => respond(&mut stream, "200 OK", "text/html; charset=utf-8", PAGE.as_bytes()),
        Route::Events => events(&mut stream, shared),
        Route::Do(action) => {
            let _ = tx.send_blocking(action);
            respond(&mut stream, "204 No Content", "text/plain", b"")
        }
        Route::Refuse(status) => respond(&mut stream, status, "text/plain", status.as_bytes()),
    };
}

/// A request as the server reads it.
#[derive(Debug, Default)]
struct Request {
    method: String,
    path: String,
    /// Names in lowercase.
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

fn read_request(reader: &mut impl BufRead) -> io::Result<Request> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "bad request");
    let mut head = 0;
    let mut line = String::new();
    let mut next_line = |line: &mut String| -> io::Result<()> {
        line.clear();
        let n = reader.by_ref().take((MAX_HEAD - head) as u64).read_line(line)?;
        head += n;
        if n == 0 || !line.ends_with('\n') {
            return Err(bad());
        }
        Ok(())
    };
    next_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else { return Err(bad()) };
    let mut request = Request { method: method.to_string(), path: path.to_string(), ..Default::default() };
    loop {
        next_line(&mut line)?;
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        let (name, value) = l.split_once(':').ok_or_else(bad)?;
        request.headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    let length: usize = request.header("content-length").map(|v| v.parse().map_err(|_| bad())).transpose()?.unwrap_or(0);
    if length > MAX_BODY {
        return Err(bad());
    }
    request.body = vec![0; length];
    reader.read_exact(&mut request.body)?;
    Ok(request)
}

/// What to do with a request.
#[derive(Debug, PartialEq)]
enum Route {
    Page,
    Events,
    Do(Action),
    Refuse(&'static str),
}

/// Checks first (the host, the token, the origin), then where it goes.
fn route(r: &Request, port: u16, token: &str) -> Route {
    let host = r.header("host").unwrap_or_default();
    if !host_ok(host, port) {
        return Route::Refuse("403 Forbidden");
    }
    // A page of another origin may not post here even if it had the token.
    if r.header("origin").is_some_and(|o| !o.eq_ignore_ascii_case(&format!("http://{host}"))) {
        return Route::Refuse("403 Forbidden");
    }
    let path = r.path.split('?').next().unwrap_or_default();
    let mut parts = path.strip_prefix('/').unwrap_or_default().splitn(2, '/');
    if !same_secret(parts.next().unwrap_or_default(), token) {
        return Route::Refuse("403 Forbidden");
    }
    let body = || serde_json::from_slice::<Value>(&r.body).ok();
    let id = |v: &Value| v.get("id").and_then(Value::as_u64);
    match (r.method.as_str(), parts.next().unwrap_or_default()) {
        ("GET", "") => Route::Page,
        ("GET", "events") => Route::Events,
        ("POST", "tick") => match body().and_then(|v| Some(Action::Tick { id: id(&v)?, done: v.get("done")?.as_bool()? })) {
            Some(a) => Route::Do(a),
            None => Route::Refuse("400 Bad Request"),
        },
        ("POST", "remove") => match body().and_then(|v| Some(Action::Remove { id: id(&v)? })) {
            Some(a) => Route::Do(a),
            None => Route::Refuse("400 Bad Request"),
        },
        (_, "" | "events" | "tick" | "remove") => Route::Refuse("405 Method Not Allowed"),
        _ => Route::Refuse("404 Not Found"),
    }
}

/// The address the page was opened at, on this port: anything else is another name pointed at
/// 127.0.0.1 (DNS rebinding) or a request not meant for this server.
fn host_ok(host: &str, port: u16) -> bool {
    let Some((name, p)) = host.rsplit_once(':') else { return false };
    p == port.to_string() && (name == "127.0.0.1" || name.eq_ignore_ascii_case("localhost"))
}

/// Whether `given` is the token, in time that does not depend on where they differ.
fn same_secret(given: &str, token: &str) -> bool {
    given.len() == token.len() && given.bytes().zip(token.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

/// Headers on every answer: nothing cached, sniffed, framed or sent on as a referrer, and the
/// page may load nothing from elsewhere. No CORS headers: other origins get nothing back.
const COMMON_HEADERS: &str = "Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'\r\n";

fn respond(stream: &mut TcpStream, status: &str, kind: &str, body: &[u8]) -> io::Result<()> {
    let head = format!("HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n{COMMON_HEADERS}Connection: close\r\n\r\n", body.len());
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// An event stream: the lists now, then again after each change, until the page goes away.
fn events(stream: &mut TcpStream, shared: &Shared) -> io::Result<()> {
    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n{COMMON_HEADERS}Connection: keep-alive\r\n\r\nretry: 2000\n\n").as_bytes())?;
    let mut seen = None;
    loop {
        let message = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if seen == Some(state.0) {
                state = shared.changed.wait_timeout(state, KEEPALIVE).unwrap_or_else(|e| e.into_inner()).0;
            }
            if seen == Some(state.0) {
                ": keepalive\n\n".to_string()
            } else {
                seen = Some(state.0);
                format!("data: {}\n\n", state.1)
            }
        };
        stream.write_all(message.as_bytes())?;
        stream.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn request(method: &str, path: &str, host: &str, body: &str) -> Request {
        Request { method: method.into(), path: path.into(), headers: vec![("host".into(), host.into())], body: body.as_bytes().to_vec() }
    }

    #[::core::prelude::v1::test]
    fn the_page_needs_the_token() {
        assert_eq!(route(&request("GET", &format!("/{TOKEN}/"), "127.0.0.1:4000", ""), 4000, TOKEN), Route::Page);
        assert_eq!(route(&request("GET", &format!("/{TOKEN}"), "localhost:4000", ""), 4000, TOKEN), Route::Page);
        assert_eq!(route(&request("GET", "/", "127.0.0.1:4000", ""), 4000, TOKEN), Route::Refuse("403 Forbidden"));
        assert_eq!(route(&request("GET", "/0123456789abcdef0123456789abcdee/events", "127.0.0.1:4000", ""), 4000, TOKEN), Route::Refuse("403 Forbidden"));
    }

    #[::core::prelude::v1::test]
    fn another_host_name_is_refused() {
        let path = format!("/{TOKEN}/events");
        assert_eq!(route(&request("GET", &path, "127.0.0.1:4000", ""), 4000, TOKEN), Route::Events);
        for host in ["evil.example:4000", "127.0.0.1:4001", "127.0.0.1", "", "0.0.0.0:4000"] {
            assert_eq!(route(&request("GET", &path, host, ""), 4000, TOKEN), Route::Refuse("403 Forbidden"), "{host}");
        }
    }

    #[::core::prelude::v1::test]
    fn another_origin_cannot_post() {
        let mut r = request("POST", &format!("/{TOKEN}/tick"), "127.0.0.1:4000", r#"{"id":3,"done":true}"#);
        r.headers.push(("origin".into(), "http://127.0.0.1:4000".into()));
        assert_eq!(route(&r, 4000, TOKEN), Route::Do(Action::Tick { id: 3, done: true }));
        r.headers[1].1 = "https://evil.example".into();
        assert_eq!(route(&r, 4000, TOKEN), Route::Refuse("403 Forbidden"));
    }

    #[::core::prelude::v1::test]
    fn posts_read_their_body() {
        let r = request("POST", &format!("/{TOKEN}/remove"), "127.0.0.1:4000", r#"{"id":7}"#);
        assert_eq!(route(&r, 4000, TOKEN), Route::Do(Action::Remove { id: 7 }));
        let r = request("POST", &format!("/{TOKEN}/tick"), "127.0.0.1:4000", r#"{"id":7}"#);
        assert_eq!(route(&r, 4000, TOKEN), Route::Refuse("400 Bad Request"));
        let r = request("GET", &format!("/{TOKEN}/tick"), "127.0.0.1:4000", "");
        assert_eq!(route(&r, 4000, TOKEN), Route::Refuse("405 Method Not Allowed"));
    }

    #[::core::prelude::v1::test]
    fn a_request_reads_with_its_body() {
        let raw = "POST /t/tick HTTP/1.1\r\nHost: 127.0.0.1:9\r\nContent-Length: 9\r\n\r\n{\"id\":1}\nextra";
        let r = read_request(&mut BufReader::new(raw.as_bytes())).unwrap();
        assert_eq!((r.method.as_str(), r.path.as_str(), r.header("host"), r.body.as_slice()), ("POST", "/t/tick", Some("127.0.0.1:9"), b"{\"id\":1}\n".as_slice()));
    }

    #[::core::prelude::v1::test]
    fn an_oversized_request_is_refused() {
        let raw = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(MAX_HEAD));
        assert!(read_request(&mut BufReader::new(raw.as_bytes())).is_err());
        let raw = format!("POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n", MAX_BODY + 1);
        assert!(read_request(&mut BufReader::new(raw.as_bytes())).is_err());
    }

    #[::core::prelude::v1::test]
    fn the_snapshot_names_the_folder_only() {
        let lists = vec![crate::tasks::TaskList {
            id: 1,
            title: "Chores".into(),
            folder: PathBuf::from("/Users/someone/game"),
            tasks: vec![crate::tasks::Task { id: 2, title: "Sign in".into(), link: Some("https://example.com".into()), ..Default::default() }],
            ..Default::default()
        }];
        let v: Value = serde_json::from_str(&snapshot(&lists)).unwrap();
        assert_eq!((v["lists"][0]["folder"].as_str(), v["lists"][0]["tasks"][0]["link"].as_str()), (Some("game"), Some("https://example.com")));
    }

    #[::core::prelude::v1::test]
    fn a_token_is_long_and_fresh() {
        let (a, b) = (new_token().unwrap(), new_token().unwrap());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }
}
