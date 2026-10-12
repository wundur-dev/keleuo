//! One window for every `keleion` command: the first one listens on a local socket, and a
//! later command hands its request (browse a folder, maybe over SSH) to it and exits, so a new
//! session lands in the open window instead of a second app.
//!
//! The socket is named after the config folder, so test runs (`KELEION_CONFIG_DIR`) never
//! reach a person's own window.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

/// What a command asks of the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// `host` or `user@host` for `--ssh`, empty for this machine.
    pub ssh: String,
    pub path: PathBuf,
    /// `--new`: start a session there instead of showing the browser.
    pub new: bool,
}

impl Request {
    fn line(&self) -> String {
        format!("browse\t{}\t{}\t{}\n", self.ssh, self.path.display(), u8::from(self.new))
    }

    fn parse(line: &str) -> Option<Self> {
        let mut parts = line.trim_end_matches('\n').split('\t');
        (parts.next()? == "browse").then_some(())?;
        let ssh = parts.next()?.to_string();
        let path = PathBuf::from(parts.next()?);
        let new = parts.next() == Some("1");
        Some(Self { ssh, path, new })
    }
}

/// The socket path: short (sockets allow about 100 bytes), per user and config folder.
pub fn socket_path() -> PathBuf {
    let mut h = DefaultHasher::new();
    keleion_core::settings::config_dir().hash(&mut h);
    let uid = unsafe { libc::getuid() };
    std::env::temp_dir().join(format!("keleion-{uid}-{:08x}.sock", h.finish() as u32))
}

/// Hand `request` to a window that is already open. True when one took it.
pub fn hand_over(request: &Request) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket_path()) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(3)));
    if stream.write_all(request.line().as_bytes()).is_err() {
        return false;
    }
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).is_ok() && reply.trim() == "ok"
}

/// Listen for later commands; each request goes to `on_request` (on a background thread).
pub fn listen(on_request: impl Fn(Request) + Send + 'static) {
    let path = socket_path();
    // A socket left by a window that crashed: nobody answers it, so it can go.
    if UnixStream::connect(&path).is_err() {
        let _ = std::fs::remove_file(&path);
    }
    let Ok(listener) = UnixListener::bind(&path) else {
        return;
    };
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            let mut reader = BufReader::new(&stream);
            if reader.read_line(&mut line).is_ok()
                && let Some(request) = Request::parse(&line)
            {
                on_request(request);
                let _ = (&stream).write_all(b"ok\n");
            }
        }
    });
}

/// Remove the socket when the window closes.
pub fn stop_listening() {
    let _ = std::fs::remove_file(socket_path());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn requests_round_trip() {
        let r = Request { ssh: "dev".into(), path: "/srv/my app".into(), new: true };
        assert_eq!(Request::parse(&r.line()), Some(r));
        assert_eq!(Request::parse("other\tx"), None);
    }
}
