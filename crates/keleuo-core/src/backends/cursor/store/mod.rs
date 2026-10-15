//! Cursor's own record of a chat: `~/.cursor/chats/<md5 of the folder>/<chat id>/` (a session
//! started over ACP: `~/.cursor/acp-sessions/<id>/`), with a `meta.json` (its
//! `schemaVersion`) and a `store.db` (SQLite). Cursor does not document it,
//! so each layout Keleuo can read has a reader of its own here (`v1`, ...), and a chat is
//! read by the reader for its `schemaVersion`, else by each reader in turn, newest first. When
//! none can read it, the caller falls back to showing no earlier messages.
//!
//! On this machine the database is opened in place, read-only (Cursor's files are never
//! written; only the records needed are read, so a chat of hundreds of megabytes, mostly file
//! snapshots, reads at once). On another machine it is copied here with its write-ahead log,
//! up to [`REMOTE_MAX`]; a larger one is not read.

mod v1;

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::agent::Event;
use crate::conversation::Conversation;
use crate::host::Host;

/// A reader of one layout of Cursor's chat database.
pub trait StoreReader: Sync {
    /// The `schemaVersion` in `meta.json` it was written for.
    fn schema(&self) -> u64;
    /// The Cursor Agent version it was worked out against (for reports when it fails).
    fn built_against(&self) -> &'static str;
    /// The chat as neutral events, in order: the person's messages, replies, tool calls and
    /// their results.
    fn events(&self, db: &rusqlite::Connection, cwd: &str) -> anyhow::Result<Vec<Event>>;
}

/// Every reader, newest first.
const READERS: &[&dyn StoreReader] = &[&v1::V1];

/// The folder Cursor keeps chat `id` in, on `host`: under the folder's md5 when the chat is
/// there, else wherever it is (Cursor names the folder after the path it saw, which may differ
/// from Keleuo's by a symlink).
pub fn chat_dir(host: &dyn Host, project: &Path, id: &str) -> Option<PathBuf> {
    let cursor = host.home_dir()?.join(".cursor");
    // A session started over ACP (`cursor-agent acp`) is kept apart, by its id alone, in the
    // same layout.
    let acp = cursor.join("acp-sessions").join(id);
    if host.is_file(&acp.join("store.db")) {
        return Some(acp);
    }
    let chats = cursor.join("chats");
    let hashed = chats.join(md5_hex(project.to_string_lossy().as_bytes())).join(id);
    if host.is_file(&hashed.join("store.db")) {
        return Some(hashed);
    }
    host.read_dir(&chats)
        .ok()?
        .into_iter()
        .filter(|e| e.is_dir)
        .map(|e| chats.join(e.name).join(id))
        .find(|dir| host.is_file(&dir.join("store.db")))
}

/// Make chat `id` of `project` one conversation for both of Cursor's ways in, on `host`: its
/// ACP sessions live in `acp-sessions/<id>`, its own interface's chats in
/// `chats/<md5 of the folder>/<id>`, in the same layout, and neither finds the other. One is
/// made a link to the other (a symbolic link; a junction on Windows), which Cursor follows
/// both ways: the one that has the chat stays. An empty one left by the other way (no
/// conversation in it) is moved aside first, never deleted. Two with a conversation each are
/// left alone (an error says so).
pub fn share(host: &dyn Host, project: &Path, id: &str) -> std::io::Result<()> {
    let cursor = host.home_dir().ok_or_else(|| std::io::Error::other("no home folder"))?.join(".cursor");
    let acp = cursor.join("acp-sessions").join(id);
    // Cursor names the folder after the path it runs in, symlinks resolved (here; another
    // machine's paths are taken as given).
    let project = if host.is_remote() { project.to_path_buf() } else { std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf()) };
    let chat = cursor.join("chats").join(md5_hex(project.to_string_lossy().as_bytes())).join(id);
    // Shared already: both resolve to one folder (here; however the platform reports links).
    if !host.is_remote()
        && let (Ok(a), Ok(c)) = (std::fs::canonicalize(&acp), std::fs::canonicalize(&chat))
        && a == c
    {
        return Ok(());
    }
    let real = |p: &Path| host.exists(p) && !host.is_file(p) && !host.is_link(p);
    let has_chat = |p: &Path| {
        host.is_file(&p.join("store.db"))
            && host
                .read_to_string(&p.join("meta.json"))
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .is_none_or(|m| m.get("hasConversation").and_then(Value::as_bool) != Some(false))
    };
    let (keep, link) = match (real(&acp), real(&chat)) {
        (true, true) => match (has_chat(&acp), has_chat(&chat)) {
            (true, true) => return Err(std::io::Error::other("Cursor has two different chats under this id; each view keeps its own")),
            (true, false) => (acp, chat),
            (false, _) => (chat, acp),
        },
        (true, false) => (acp, chat),
        (false, true) => (chat, acp),
        // Not made yet by either, or linked already.
        (false, false) => return Ok(()),
    };
    if host.is_link(&link) {
        return Ok(());
    }
    if host.exists(&link) {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        host.rename(&link, &link.with_file_name(format!("{id}.keleuo-empty-{stamp}")))?;
    }
    if let Some(parent) = link.parent() {
        host.create_dir_all(parent)?;
    }
    host.link_dir(&keep, &link)
}

/// Chat `id` of `project`, read from Cursor's database. An error when it is not there or no
/// reader could make sense of it.
pub fn load(host: &dyn Host, project: &Path, id: &str) -> anyhow::Result<Conversation> {
    let events = read(host, project, id)?;
    Ok(conversation(&project.to_string_lossy(), id, events))
}

/// Chat `id` of `project` as neutral events, from Cursor's database.
pub fn read(host: &dyn Host, project: &Path, id: &str) -> anyhow::Result<Vec<Event>> {
    let dir = chat_dir(host, project, id).ok_or_else(|| anyhow::anyhow!("Cursor has no chat {id}"))?;
    let schema = host
        .read_to_string(&dir.join("meta.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|m| m.get("schemaVersion")?.as_u64());
    let (db, _copy) = open(host, &dir)?;
    let cwd = project.to_string_lossy();
    let mut failures = Vec::new();
    for reader in ordered(schema) {
        match reader.events(&db, &cwd).and_then(check) {
            Ok(events) => return Ok(events),
            Err(e) => failures.push(format!("reader v{} (built against {}): {e}", reader.schema(), reader.built_against())),
        }
    }
    anyhow::bail!("could not read Cursor chat {id} (schemaVersion {schema:?}): {}", failures.join("; "))
}

/// How far along chat `id`'s database is (its size and its write-ahead log's): what changes
/// when Cursor writes a turn. `None` while it has none.
pub fn written(host: &dyn Host, project: &Path, id: &str) -> Option<(u64, u64)> {
    let dir = chat_dir(host, project, id)?;
    Some((host.len(&dir.join("store.db")).ok()?, host.len(&dir.join("store.db-wal")).unwrap_or(0)))
}

/// The readers to try for `schema`: its own first, then the rest, newest first.
fn ordered(schema: Option<u64>) -> Vec<&'static dyn StoreReader> {
    let mut readers: Vec<&'static dyn StoreReader> = READERS.to_vec();
    readers.sort_by_key(|r| (Some(r.schema()) != schema, std::cmp::Reverse(r.schema())));
    readers
}

/// A reading that found none of the person's messages is not trusted: the layout has likely
/// changed under the reader.
fn check(events: Vec<Event>) -> anyhow::Result<Vec<Event>> {
    if events.iter().any(|e| matches!(e, Event::UserText(_))) {
        Ok(events)
    } else {
        anyhow::bail!("no messages found")
    }
}

/// The events applied as a finished conversation (no live clocks, nothing running).
pub fn conversation(cwd: &str, id: &str, events: Vec<Event>) -> Conversation {
    let mut chat = Conversation::new(cwd);
    chat.replaying = true;
    chat.apply_all(events);
    chat.session_id = Some(id.to_string());
    chat.busy = false;
    chat.status = None;
    chat.replaying = false;
    chat
}

/// The largest database copied from another machine to be read (bytes).
pub const REMOTE_MAX: u64 = 64 << 20;

/// Chat folder `dir`'s database, read-only: in place here, else a copy (kept while it is read).
fn open(host: &dyn Host, dir: &Path) -> anyhow::Result<(rusqlite::Connection, Option<Copy>)> {
    use rusqlite::OpenFlags;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if !host.is_remote() {
        let path = dir.join("store.db");
        let db = rusqlite::Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        // Cursor may be writing a turn: wait for it rather than fail.
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        return Ok((db, None));
    }
    let size = host.len(&dir.join("store.db"))?;
    anyhow::ensure!(size <= REMOTE_MAX, "its record is {} MB, too large to fetch from the other machine", size >> 20);
    let copy = Copy::of(host, dir)?;
    let db = rusqlite::Connection::open_with_flags(&copy.db, flags)?;
    Ok((db, Some(copy)))
}

/// The database copied into a folder of its own, removed when dropped.
struct Copy {
    dir: PathBuf,
    db: PathBuf,
}

impl Copy {
    fn of(host: &dyn Host, chat: &Path) -> anyhow::Result<Self> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("keleuo-cursor-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let copy = Self { db: dir.join("store.db"), dir };
        std::fs::write(&copy.db, host.read(&chat.join("store.db"))?)?;
        // What Cursor wrote but has not folded into the database yet.
        if let Ok(wal) = host.read(&chat.join("store.db-wal")) {
            std::fs::write(copy.dir.join("store.db-wal"), wal)?;
        }
        Ok(copy)
    }
}

impl Drop for Copy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// MD5 of `data` as lower-case hex: Cursor names a folder's chat directory with it.
fn md5_hex(data: &[u8]) -> String {
    use md5::Digest as _;
    md5::Md5::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// The fields of a protobuf message, as `(field number, wire type, value)`; varints as their
/// number, lengths as their bytes. `None` when it does not parse.
pub(crate) fn protobuf_fields(mut b: &[u8]) -> Option<Vec<(u64, u8, Field<'_>)>> {
    fn varint(b: &mut &[u8]) -> Option<u64> {
        let (mut v, mut shift) = (0u64, 0);
        loop {
            let (&x, rest) = b.split_first()?;
            *b = rest;
            v |= u64::from(x & 0x7f) << shift;
            if x < 0x80 {
                return Some(v);
            }
            shift += 7;
            if shift > 63 {
                return None;
            }
        }
    }
    let mut out = Vec::new();
    while !b.is_empty() {
        let key = varint(&mut b)?;
        let (field, wire) = (key >> 3, (key & 7) as u8);
        let value = match wire {
            0 => Field::Int(varint(&mut b)?),
            2 => {
                let n = usize::try_from(varint(&mut b)?).ok()?;
                let (v, rest) = (b.get(..n)?, b.get(n..)?);
                b = rest;
                Field::Bytes(v)
            }
            1 | 5 => {
                let n = if wire == 1 { 8 } else { 4 };
                let (v, rest) = (b.get(..n)?, b.get(n..)?);
                b = rest;
                Field::Bytes(v)
            }
            _ => return None,
        };
        out.push((field, wire, value));
    }
    Some(out)
}

/// One protobuf field's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Field<'a> {
    Int(u64),
    Bytes(&'a [u8]),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_cursors_folder_names() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        // A real chat folder: md5 of the path Cursor ran in.
        assert_eq!(md5_hex(b"/private/var/folders/fb/8ktdcknd7t5g2nd_d8rkx81h0000gn/T/cursor-probe"), "4ceed62e7a512cac3ad6c5e09f19c50c");
    }

    /// Every Cursor chat on this machine reads (prints a line per chat; nothing of them is
    /// kept). `cargo test -p keleuo-core every_cursor_chat -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn every_cursor_chat_on_this_machine_reads() {
        let host = crate::host::LocalHost;
        let cursor = dirs::home_dir().unwrap().join(".cursor");
        let (mut read, mut empty, mut failed) = (0, 0, Vec::new());
        // Chats under a folder per project, and sessions started over ACP on their own.
        let mut found: Vec<std::path::PathBuf> = std::fs::read_dir(cursor.join("chats"))
            .into_iter()
            .flatten()
            .flatten()
            .flat_map(|folder| std::fs::read_dir(folder.path()).into_iter().flatten().flatten().map(|c| c.path()))
            .collect();
        found.extend(std::fs::read_dir(cursor.join("acp-sessions")).into_iter().flatten().flatten().map(|c| c.path()));
        {
            for chat in found {
                let id = chat.file_name().unwrap_or_default().to_string_lossy().to_string();
                // A session that never got a message has no database yet.
                if !chat.join("store.db").exists() {
                    empty += 1;
                    continue;
                }
                let cwd = std::fs::read_to_string(chat.join("meta.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                    .and_then(|m| m.get("cwd")?.as_str().map(str::to_string))
                    .unwrap_or_default();
                match load(&host, Path::new(&cwd), &id) {
                    Ok(c) => {
                        read += 1;
                        let users = c.items.iter().filter(|i| matches!(i, crate::conversation::Item::User(_))).count();
                        eprintln!("ok   {id}: {} items, {users} messages", c.items.len());
                    }
                    Err(e) if e.to_string().contains("no messages found") => {
                        empty += 1;
                        eprintln!("none {id}: {e}");
                    }
                    Err(e) => {
                        eprintln!("FAIL {id}: {e}");
                        failed.push(id);
                    }
                }
            }
        }
        eprintln!("read {read}, without messages {empty}, failed {}", failed.len());
        assert!(failed.is_empty(), "failed: {failed:?}");
    }

    #[test]
    fn a_chat_is_shared_by_both_ways_in() {
        // A home of its own: `<tmp>/keleuo-share-N/.cursor`.
        let home = std::env::temp_dir().join(format!("keleuo-share-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let root = home.join(".cursor");
        let project = Path::new("/p");
        let hashed = root.join("chats").join(md5_hex(b"/p"));
        let write = |dir: &Path, conversation: bool| {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("store.db"), b"db").unwrap();
            std::fs::write(dir.join("meta.json"), format!("{{\"schemaVersion\":1,\"hasConversation\":{conversation}}}")).unwrap();
        };
        // Started over ACP: the interface's place becomes a link to it.
        write(&root.join("acp-sessions/a"), true);
        let host = HomeAt(root.parent().unwrap().to_path_buf());
        share(&host, project, "a").unwrap();
        assert!(hashed.join("a").is_symlink());
        assert_eq!(std::fs::read_link(hashed.join("a")).unwrap(), root.join("acp-sessions/a"));
        share(&host, project, "a").unwrap();
        // Started in the interface: ACP's place becomes a link to it.
        write(&hashed.join("b"), true);
        share(&host, project, "b").unwrap();
        assert!(root.join("acp-sessions/b").is_symlink());
        // An empty twin is moved aside, not deleted.
        write(&root.join("acp-sessions/c"), true);
        write(&hashed.join("c"), false);
        share(&host, project, "c").unwrap();
        assert!(hashed.join("c").is_symlink());
        assert!(std::fs::read_dir(&hashed).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with("c.keleuo-empty-")));
        // Two real chats: left alone.
        write(&root.join("acp-sessions/d"), true);
        write(&hashed.join("d"), true);
        assert!(share(&host, project, "d").is_err());
        assert!(!hashed.join("d").is_symlink());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// This machine, with its home folder elsewhere.
    struct HomeAt(std::path::PathBuf);

    impl Host for HomeAt {
        fn label(&self) -> String {
            "test".into()
        }
        fn is_remote(&self) -> bool {
            false
        }
        fn home_dir(&self) -> Option<std::path::PathBuf> {
            Some(self.0.clone())
        }
        fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
            crate::host::LocalHost.read(path)
        }
        fn write(&self, path: &Path, contents: &[u8]) -> std::io::Result<()> {
            crate::host::LocalHost.write(path, contents)
        }
        fn create_dir_all(&self, path: &Path) -> std::io::Result<()> {
            crate::host::LocalHost.create_dir_all(path)
        }
        fn exists(&self, path: &Path) -> bool {
            crate::host::LocalHost.exists(path)
        }
        fn is_file(&self, path: &Path) -> bool {
            crate::host::LocalHost.is_file(path)
        }
        fn is_link(&self, path: &Path) -> bool {
            crate::host::LocalHost.is_link(path)
        }
        fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
            crate::host::LocalHost.rename(from, to)
        }
        fn link_dir(&self, target: &Path, link: &Path) -> std::io::Result<()> {
            crate::host::LocalHost.link_dir(target, link)
        }
        fn read_dir(&self, path: &Path) -> std::io::Result<Vec<crate::host::DirEntry>> {
            crate::host::LocalHost.read_dir(path)
        }
        fn list_files(&self, root: &Path, max: usize) -> Vec<(std::path::PathBuf, bool)> {
            crate::host::LocalHost.list_files(root, max)
        }
        fn watch(&self, root: &Path, changes: std::sync::mpsc::Sender<crate::host::Change>) -> Option<crate::host::Watch> {
            crate::host::LocalHost.watch(root, changes)
        }
        fn shell_command(&self, cwd: &Path) -> crate::host::CommandSpec {
            crate::host::LocalHost.shell_command(cwd)
        }
        fn program_command(&self, spec: &crate::host::ProgramSpec) -> crate::host::CommandSpec {
            crate::host::LocalHost.program_command(spec)
        }
        fn git(&self, dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
            crate::host::LocalHost.git(dir, args)
        }
    }

    #[test]
    fn a_chats_own_schema_is_tried_first() {
        assert_eq!(ordered(Some(1))[0].schema(), 1);
        assert_eq!(ordered(Some(99)).len(), READERS.len());
    }
}
