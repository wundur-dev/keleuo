//! The window's end of the agents' hooks: a local socket that `keleuo-hook` (run by an
//! agent's terminal interface on its hook events) connects to, one line per call. Calls are
//! routed to the session whose token they carry; a permission request keeps its connection
//! open until the session answers it (or drops it, when the person answered in the terminal).
//!
//! Each session gets a token and a settings file (in a folder of this window's own under the
//! system's temporary folder) that names `keleuo-hook` with this socket and that token; the
//! agent is started with it. Nothing in the person's own agent settings is touched.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::*;
use interprocess::local_socket::traits::ListenerExt as _;
use serde_json::Value;

use crate::agent_session::AgentSession;
use crate::local_socket;

/// One hook call.
pub struct Call {
    pub token: String,
    pub event: String,
    pub payload: Value,
    /// The open connection of a call that waits for an answer.
    pub reply: Option<Reply>,
}

/// Where a waiting hook's answer goes; dropping it without answering lets the agent ask in
/// its own interface.
pub struct Reply(interprocess::local_socket::Stream);

impl Reply {
    pub fn send(mut self, answer: &str) {
        let _ = self.0.write_all(format!("{answer}\n").as_bytes());
    }
}

/// The window's hook socket and the sessions it serves.
#[derive(Clone, Default)]
pub struct Hooks(pub Rc<RefCell<State>>);

#[derive(Default)]
pub struct State {
    /// The socket, when it could be opened.
    pub socket: Option<PathBuf>,
    /// The helper program next to this one.
    pub helper: Option<PathBuf>,
    /// The tool server (MCP) next to this one: the messaging tools (see `messaging`).
    pub mcp: Option<PathBuf>,
    /// The folder for the sessions' settings files.
    pub dir: PathBuf,
    sessions: HashMap<String, WeakEntity<AgentSession>>,
    next: u64,
}

impl Global for Hooks {}

/// Open the socket and start serving it. Without a socket (or the helper), sessions run
/// without hooks: permission prompts are answered in the terminal view only.
pub fn init(cx: &mut App) {
    let dir = std::env::temp_dir().join(format!("keleuo-{}", std::process::id()));
    // Folders of windows that ended without tidying up (killed), off the UI thread.
    std::thread::spawn(remove_stale);
    let helper = std::env::current_exe()
        .ok()
        .map(|exe| exe.with_file_name(if cfg!(windows) { "keleuo-hook.exe" } else { "keleuo-hook" }))
        .filter(|p| p.is_file());
    let mcp = std::env::current_exe()
        .ok()
        .map(|exe| exe.with_file_name(if cfg!(windows) { "keleuo-mcp.exe" } else { "keleuo-mcp" }))
        .filter(|p| p.is_file());
    let mut state = State { dir: dir.clone(), helper, mcp, ..Default::default() };
    if state.helper.is_some() && std::fs::create_dir_all(&dir).is_ok() {
        let (tx, rx) = async_channel::unbounded::<Call>();
        let socket = local_socket::address(&dir.join("hooks.sock"), &format!("keleuo-{}-hooks", std::process::id()));
        if listen(&socket, tx) {
            state.socket = Some(socket);
            cx.spawn(async move |cx: &mut AsyncApp| {
                while let Ok(call) = rx.recv().await {
                    cx.update(|cx| route(call, cx));
                }
            })
            .detach();
        }
    }
    if state.socket.is_none() {
        eprintln!("keleuo: no hook socket; permission prompts are answered in the terminal view");
    }
    cx.set_global(Hooks(Rc::new(RefCell::new(state))));
}

/// Remove `keleuo-<pid>` folders (or ones of the app's earlier names) whose window process is
/// gone. On Windows the socket is a named pipe, not a file in the folder: any such folder.
fn remove_stale() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if let Some(pid) = ["keleuo-", "keleion-", "workbench-"].iter().find_map(|p| name.strip_prefix(p)).and_then(|p| p.parse::<u32>().ok())
            && pid != std::process::id()
            && (e.path().join("hooks.sock").exists() || cfg!(windows) && e.path().is_dir())
            && !keleuo_core::host::process_alive(pid)
        {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// The folder and socket are this process's own: remove them on the way out.
pub fn cleanup(cx: &App) {
    if let Some(h) = cx.try_global::<Hooks>() {
        let _ = std::fs::remove_dir_all(&h.0.borrow().dir);
    }
}

fn listen(socket: &Path, tx: async_channel::Sender<Call>) -> bool {
    // The folder is this process's own: a socket file already there is from an earlier
    // process with the same id, and goes.
    let Ok(listener) = local_socket::listen(socket, true) else { return false };
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut line = String::new();
                if BufReader::new(&stream).read_line(&mut line).is_err() {
                    return;
                }
                let Ok(v) = serde_json::from_str::<Value>(&line) else { return };
                let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                let event = s("event");
                // A permission request and a tool call wait for their answer.
                let waits = event == "PermissionRequest" || event == "mcp";
                let call = Call {
                    token: s("token"),
                    payload: v.get("payload").cloned().unwrap_or(Value::Null),
                    reply: waits.then_some(Reply(stream)),
                    event,
                };
                let _ = tx.send_blocking(call);
            });
        }
    });
    true
}

fn route(call: Call, cx: &mut App) {
    let target = cx
        .try_global::<Hooks>()
        .and_then(|h| h.0.borrow().sessions.get(&call.token).cloned());
    let Some(session) = target.and_then(|w| w.upgrade()) else { return };
    if call.event == "mcp" {
        crate::messaging::tool_call(session, call, cx);
    } else {
        session.update(cx, |s, cx| s.on_hook(call, cx));
    }
}

/// A session's hooks: its token and the settings file to start its agent with. `None` when
/// the window has no hook socket or the backend no hooks.
pub struct Registration {
    pub settings: PathBuf,
    /// The tool servers file to start its agent with (the messaging tools), when the window
    /// has its tool server.
    pub mcp: Option<PathBuf>,
    token: String,
    /// The window's socket.
    socket: PathBuf,
}

/// `keleuo-hook` for an agent on another machine (see the file).
const REMOTE_HELPER: &str = include_str!("bin/keleuo-hook.py");

impl Registration {
    pub fn token(&self) -> &str {
        &self.token
    }

    /// For an agent on another machine, which has no `keleuo-hook`: the settings file to start
    /// it with and the files to write there first (in `dir`: the Python helper, and settings
    /// naming `socket`, a socket there that its connection forwards to the window's). Also
    /// the window's socket, the far end of that forward.
    pub fn remote(&self, backend: &dyn keleuo_core::agent::Backend, dir: &Path, socket: &Path) -> Option<RemoteHooks> {
        // Each launch its own copy of both: they go when it ends, and other sessions may
        // still be running.
        let name = socket.file_stem().map_or_else(|| self.token.clone(), |s| s.to_string_lossy().into_owned());
        let (helper, settings) = (dir.join(format!("{name}.py")), dir.join(format!("{name}.json")));
        let quote = |p: &Path| keleuo_core::host::shell_quote(&p.to_string_lossy());
        let command = |event: &str| format!("python3 {} {} {} {event}", quote(&helper), quote(socket), self.token);
        let text = backend.hook_settings(&command)?;
        Some(RemoteHooks {
            settings: settings.clone(),
            files: vec![(helper, REMOTE_HELPER.to_string()), (settings, text)],
            here: self.socket.clone(),
        })
    }
}

/// What [`Registration::remote`] gives.
pub struct RemoteHooks {
    pub settings: PathBuf,
    pub files: Vec<(PathBuf, String)>,
    pub here: PathBuf,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.settings);
        if let Some(mcp) = &self.mcp {
            let _ = std::fs::remove_file(mcp);
        }
    }
}

pub fn register(
    session: WeakEntity<AgentSession>,
    backend: &dyn keleuo_core::agent::Backend,
    cx: &mut App,
) -> Option<Registration> {
    let hooks = cx.try_global::<Hooks>()?.clone();
    let mut state = hooks.0.borrow_mut();
    let (socket, helper) = (state.socket.clone()?, state.helper.clone()?);
    state.next += 1;
    let token = format!("s{}-{}", std::process::id(), state.next);
    let command = |event: &str| {
        format!(
            "{} {} {} {event}",
            keleuo_core::host::shell_quote(&helper.to_string_lossy()),
            keleuo_core::host::shell_quote(&socket.to_string_lossy()),
            token
        )
    };
    let text = backend.hook_settings(&command)?;
    let settings = state.dir.join(format!("{token}.json"));
    std::fs::write(&settings, text).ok()?;
    // The tool server, named with the same socket and token.
    let mcp = state.mcp.clone().and_then(|program| {
        let config = serde_json::json!({ "mcpServers": { "keleuo": {
            "type": "stdio",
            "command": program.to_string_lossy(),
            "args": [socket.to_string_lossy(), token],
        } } });
        let path = state.dir.join(format!("{token}-mcp.json"));
        std::fs::write(&path, config.to_string()).ok().map(|_| path)
    });
    state.sessions.insert(token.clone(), session);
    Some(Registration { settings, mcp, token, socket })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// What `keleuo-hook` does, by hand: a permission call keeps its connection, and the
    /// answer comes back on it; other calls do not wait.
    #[::core::prelude::v1::test]
    fn calls_arrive_and_permission_answers_go_back() {
        let dir = std::env::temp_dir().join(format!("wb-hooks-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = local_socket::address(&dir.join("t.sock"), &format!("wb-hooks-test-{}", std::process::id()));
        let (tx, rx) = async_channel::unbounded::<Call>();
        assert!(listen(&socket, tx));

        let mut c = local_socket::connect(&socket).unwrap();
        c.write_all(b"{\"token\":\"s1-1\",\"event\":\"Stop\",\"payload\":{}}\n").unwrap();
        let call = rx.recv_blocking().unwrap();
        assert_eq!((call.token.as_str(), call.event.as_str()), ("s1-1", "Stop"));
        assert!(call.reply.is_none());

        let mut c = local_socket::connect(&socket).unwrap();
        c.write_all(b"{\"token\":\"s1-1\",\"event\":\"PermissionRequest\",\"payload\":{\"tool_name\":\"Bash\"}}\n").unwrap();
        let call = rx.recv_blocking().unwrap();
        assert_eq!(call.payload["tool_name"], "Bash");
        call.reply.expect("waits").send("{\"ok\":true}");
        let mut answer = String::new();
        c.read_to_string(&mut answer).unwrap();
        assert_eq!(answer.trim(), "{\"ok\":true}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
