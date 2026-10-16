//! Local sockets on every OS, through `interprocess`: Unix domain sockets on macOS and Linux,
//! named pipes on Windows. A socket's address is a path either way: a file on Unix (so it can
//! be tidied, and forwarded over ssh), `\\.\pipe\<name>` on Windows; `keleuo-hook` is given
//! the same path on its command line and opens it the same way.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{GenericFilePath, Listener, ListenerOptions, Name, Stream, ToFsName};

/// The address of a socket that is the file `unix` on Unix, and the named pipe `name` on
/// Windows (pipes live in a namespace of their own and have no file to tidy).
pub fn address(unix: &Path, name: &str) -> PathBuf {
    if cfg!(windows) { PathBuf::from(format!(r"\\.\pipe\{name}")) } else { unix.to_path_buf() }
}

fn name(address: &Path) -> io::Result<Name<'_>> {
    address.to_fs_name::<GenericFilePath>()
}

/// Connect to the socket at `address`.
pub fn connect(address: &Path) -> io::Result<Stream> {
    Stream::connect(name(address)?)
}

/// Listen at `address`. With `replace`, a socket file already there (left by a process that
/// ended without tidying up) is removed first; a named pipe goes with its process, so there is
/// never one to replace.
pub fn listen(address: &Path, replace: bool) -> io::Result<Listener> {
    ListenerOptions::new()
        .name(name(address)?)
        .try_overwrite(replace)
        .max_spin_time(Duration::from_secs(1))
        .create_sync()
}

/// Remove the socket at `address` (a file on Unix; nothing to do for a named pipe).
pub fn remove(address: &Path) {
    if cfg!(unix) {
        let _ = std::fs::remove_file(address);
    }
}
