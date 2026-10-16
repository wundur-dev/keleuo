//! One window for every `keleuo` command: the first one listens on a local socket, and a
//! later command hands its request (browse a folder, maybe over SSH) to it and exits, so a new
//! session lands in the open window instead of a second app.
//!
//! The socket is named after the config folder, so test runs (`KELEUO_CONFIG_DIR`) never
//! reach a person's own window. A Unix domain socket on macOS and Linux, a named pipe on
//! Windows (see `local_socket`).

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use interprocess::local_socket::traits::{ListenerExt as _, Stream as _};

use crate::local_socket;

/// What a command asks of the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// `host` or `user@host` for `--ssh`, empty for this machine.
    pub ssh: String,
    pub path: PathBuf,
    /// `--new`: start a session there instead of showing the browser.
    pub new: bool,
    /// `--quit`: quit the open window (asking first if a session is working).
    pub quit: bool,
}

impl Request {
    fn line(&self) -> String {
        if self.quit {
            return "quit\n".into();
        }
        format!("browse\t{}\t{}\t{}\n", self.ssh, self.path.display(), u8::from(self.new))
    }

    fn parse(line: &str) -> Option<Self> {
        if line.trim_end() == "quit" {
            return Some(Self { ssh: String::new(), path: PathBuf::new(), new: false, quit: true });
        }
        let mut parts = line.trim_end_matches('\n').split('\t');
        (parts.next()? == "browse").then_some(())?;
        let ssh = parts.next()?.to_string();
        let path = PathBuf::from(parts.next()?);
        let new = parts.next() == Some("1");
        Some(Self { ssh, path, new, quit: false })
    }
}

/// The socket's address: short (Unix sockets allow about 100 bytes), per user and config
/// folder.
pub fn socket_path() -> PathBuf {
    let mut h = DefaultHasher::new();
    keleuo_core::settings::config_dir().hash(&mut h);
    let name = format!("keleuo-{}-{:08x}", user(), h.finish() as u32);
    local_socket::address(&std::env::temp_dir().join(format!("{name}.sock")), &name)
}

/// Who this user is, for the socket's name. On Unix the temporary folder may be shared
/// (`/tmp`); on Windows named pipes are shared by everyone on the machine.
#[cfg(unix)]
fn user() -> String {
    unsafe { libc::getuid() }.to_string()
}

#[cfg(not(unix))]
fn user() -> String {
    let name = std::env::var("USERNAME").unwrap_or_default();
    name.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')).collect()
}

/// Hand `request` to a window that is already open. True when one took it.
pub fn hand_over(request: &Request) -> bool {
    hand_over_to(&socket_path(), request)
}

fn hand_over_to(socket: &Path, request: &Request) -> bool {
    let Ok(mut stream) = local_socket::connect(socket) else {
        return false;
    };
    let _ = stream.set_recv_timeout(Some(std::time::Duration::from_secs(3)));
    if stream.write_all(request.line().as_bytes()).is_err() {
        return false;
    }
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).is_ok() && reply.trim() == "ok"
}

/// Listen for later commands; each request goes to `on_request` (on a background thread).
pub fn listen(on_request: impl Fn(Request) + Send + 'static) {
    listen_on(&socket_path(), on_request);
}

/// [`listen`] at `socket`; false when it could not.
fn listen_on(socket: &Path, on_request: impl Fn(Request) + Send + 'static) -> bool {
    // A socket left by a window that crashed: nobody answers it, so it can go. One that
    // answers is another window's, and stays.
    let stale = local_socket::connect(socket).is_err();
    let Ok(listener) = local_socket::listen(socket, stale) else {
        return false;
    };
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_ok()
                && let Some(request) = Request::parse(&line)
            {
                on_request(request);
                let _ = (&stream).write_all(b"ok\n");
            }
        }
    });
    true
}

/// Remove the socket when the window closes.
pub fn stop_listening() {
    local_socket::remove(&socket_path());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn requests_round_trip() {
        let q = Request { ssh: String::new(), path: PathBuf::new(), new: false, quit: true };
        assert_eq!(Request::parse(&q.line()), Some(q));
        let r = Request { ssh: "dev".into(), path: "/srv/my app".into(), new: true, quit: false };
        assert_eq!(Request::parse(&r.line()), Some(r));
        assert_eq!(Request::parse("other\tx"), None);
    }

    /// A second command's request reaches the listening window, which says `ok`; a socket
    /// left by a window that crashed does not stop the next one listening, and a live
    /// window's socket is not taken over.
    #[::core::prelude::v1::test]
    fn requests_are_handed_over() {
        let name = format!("kl-inst-test-{}", std::process::id());
        let socket = local_socket::address(&std::env::temp_dir().join(format!("{name}.sock")), &name);
        assert!(!hand_over_to(&socket, &Request { ssh: String::new(), path: "/x".into(), new: false, quit: false }));

        // What a crashed window leaves: a socket file nobody listens on (on Unix; a named
        // pipe goes with its process).
        let mut crashed = local_socket::listen(&socket, true).unwrap();
        interprocess::local_socket::traits::Listener::do_not_reclaim_name_on_drop(&mut crashed);
        drop(crashed);

        let (tx, rx) = std::sync::mpsc::channel();
        assert!(listen_on(&socket, move |r| tx.send(r).unwrap()));
        let r = Request { ssh: "dev".into(), path: "/srv/my app".into(), new: true, quit: false };
        assert!(hand_over_to(&socket, &r));
        assert_eq!(rx.recv().unwrap(), r);

        // A second window does not take the first one's socket.
        assert!(!listen_on(&socket, |_| {}));
        assert!(hand_over_to(&socket, &r));
        assert_eq!(rx.recv().unwrap(), r);
        local_socket::remove(&socket);
    }
}
