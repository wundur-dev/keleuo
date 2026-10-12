//! Where a project lives. Every project operation goes through a [`Host`]: listing and reading
//! files, reading and writing skills, watching for changes, the shell for the terminal and the
//! `claude` command. [`LocalHost`] is this machine; [`SshHost`] reaches another one over `ssh`.
//!
//! All methods block: the window calls them from background threads, never the UI thread.
//!
//! Remote plan (see the README): today [`SshHost`] runs one `ssh` command per operation over a
//! shared control connection, which is fine for an afternoon but not fast. The next step is a
//! small headless helper binary started on the remote over `ssh`, speaking a compact protocol
//! over its stdin and stdout (list, read, write, watch, spawn), so one connection carries
//! everything and file watching works remotely.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;


/// A program to start: locally, or `ssh` for a remote host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Working directory on this machine (`None` for remote commands, which `cd` themselves).
    pub cwd: Option<PathBuf>,
    /// Extra environment variables.
    pub env: Vec<(String, String)>,
}

/// A program an agent backend wants run on the host: by name (found on the host's PATH and in
/// the usual install places) or by path, in a folder, with arguments and environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgramSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Variables to remove from the environment it inherits.
    pub unset: Vec<String>,
    pub cwd: PathBuf,
}

impl CommandSpec {
    pub fn to_command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args);
        cmd.envs(self.env.iter().map(|(k, v)| (k, v)));
        if let Some(cwd) = &self.cwd {
            cmd.current_dir(cwd);
        }
        cmd
    }
}

/// One entry of a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

/// Something changed under a watched folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub paths: Vec<PathBuf>,
}

/// Keeps a watch alive; dropping it stops watching.
pub struct Watch(#[allow(dead_code)] Box<dyn Send>);

pub trait Host: Send + Sync {
    /// A short label for the window title: `local` or the ssh destination.
    fn label(&self) -> String;
    fn is_remote(&self) -> bool;
    /// The user's home folder on the host.
    fn home_dir(&self) -> Option<PathBuf>;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        String::from_utf8(self.read(path)?)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
    /// The bytes of `path` from `offset` on (a file being appended to, read as it grows).
    fn read_from(&self, path: &Path, offset: u64) -> io::Result<Vec<u8>> {
        let mut bytes = self.read(path)?;
        let start = (offset as usize).min(bytes.len());
        Ok(bytes.split_off(start))
    }
    /// Each line of `path` from byte `from` up to `to` (the end when `None`), with the offset
    /// it starts at, read as a stream where the host can (a session file can be hundreds of
    /// megabytes). `f` returns false to stop.
    fn for_each_line(&self, path: &Path, from: u64, to: Option<u64>, f: &mut dyn FnMut(u64, &str) -> bool) -> io::Result<()> {
        let bytes = self.read_from(path, from)?;
        let mut offset = from;
        for line in bytes.split(|b| *b == b'\n') {
            if to.is_some_and(|t| offset >= t) {
                break;
            }
            let start = offset;
            offset += line.len() as u64 + 1;
            if !line.is_empty() && !f(start, &String::from_utf8_lossy(line)) {
                break;
            }
        }
        Ok(())
    }
    /// The length of `path` in bytes.
    fn len(&self, path: &Path) -> io::Result<u64> {
        self.read(path).map(|b| b.len() as u64)
    }
    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()>;
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    fn exists(&self, path: &Path) -> bool;
    fn is_file(&self, path: &Path) -> bool;
    /// Modification time in seconds since the epoch, when the host can tell.
    fn modified(&self, _path: &Path) -> Option<u64> {
        None
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>>;
    /// Every path under `root` git would not ignore, relative to `root`, with `is_dir`.
    fn list_files(&self, root: &Path, max: usize) -> Vec<(PathBuf, bool)>;
    /// Report changes under `root` until the returned [`Watch`] is dropped. `None` when the
    /// host cannot watch (the window then refreshes on Claude's edits only).
    fn watch(&self, root: &Path, changes: Sender<Change>) -> Option<Watch>;
    /// The shell for the terminal, started in `cwd`.
    fn shell_command(&self, cwd: &Path) -> CommandSpec;
    /// An agent program (any backend's), started in `spec.cwd`.
    fn program_command(&self, spec: &ProgramSpec) -> CommandSpec;
    /// The current git branch of the repository at `root`, if any.
    fn git_branch(&self, root: &Path) -> Option<String> {
        let head = self.read_to_string(&root.join(".git/HEAD")).ok()?;
        branch_from_head(&head)
    }
}

/// `ref: refs/heads/main` gives `main`; a detached head gives its short hash.
pub fn branch_from_head(head: &str) -> Option<String> {
    let head = head.trim();
    if let Some(r) = head.strip_prefix("ref:") {
        let r = r.trim();
        return Some(r.strip_prefix("refs/heads/").unwrap_or(r).to_string());
    }
    (head.len() >= 7).then(|| head[..7].to_string())
}

/// This machine.
#[derive(Clone, Debug, Default)]
pub struct LocalHost;

impl Host for LocalHost {
    fn label(&self) -> String {
        "local".into()
    }

    fn is_remote(&self) -> bool {
        false
    }

    fn home_dir(&self) -> Option<PathBuf> {
        dirs::home_dir()
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    fn read_from(&self, path: &Path, offset: u64) -> io::Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(path)?;
        f.seek(SeekFrom::Start(offset))?;
        let mut out = Vec::new();
        f.read_to_end(&mut out)?;
        Ok(out)
    }

    fn len(&self, path: &Path) -> io::Result<u64> {
        Ok(std::fs::metadata(path)?.len())
    }

    fn for_each_line(&self, path: &Path, from: u64, to: Option<u64>, f: &mut dyn FnMut(u64, &str) -> bool) -> io::Result<()> {
        use std::io::{BufRead, Seek, SeekFrom};
        let mut file = std::fs::File::open(path)?;
        file.seek(SeekFrom::Start(from))?;
        let mut reader = io::BufReader::with_capacity(1 << 20, file);
        let mut buf = Vec::new();
        let mut offset = from;
        loop {
            if to.is_some_and(|t| offset >= t) {
                break;
            }
            buf.clear();
            let n = reader.read_until(b'\n', &mut buf)?;
            if n == 0 {
                break;
            }
            let start = offset;
            offset += n as u64;
            let line = buf.strip_suffix(b"\n").unwrap_or(&buf);
            if !line.is_empty() && !f(start, &String::from_utf8_lossy(line)) {
                break;
            }
        }
        Ok(())
    }

    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        std::fs::write(path, contents)
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir_all(path)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn modified(&self, path: &Path) -> Option<u64> {
        let m = std::fs::metadata(path).ok()?.modified().ok()?;
        Some(m.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs())
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        Ok(std::fs::read_dir(path)?
            .flatten()
            .map(|e| DirEntry {
                name: e.file_name().to_string_lossy().to_string(),
                // Follows symlinks, as skills folders are often linked.
                is_dir: e.path().is_dir(),
            })
            .collect())
    }

    fn list_files(&self, root: &Path, max: usize) -> Vec<(PathBuf, bool)> {
        let walker = ignore::WalkBuilder::new(root)
            .hidden(false)
            .git_ignore(true)
            .git_exclude(true)
            .git_global(true)
            .require_git(false)
            .filter_entry(|e| e.file_name() != ".git")
            .sort_by_file_name(|a, b| a.cmp(b))
            .build();
        let mut out = Vec::new();
        for entry in walker.flatten() {
            if entry.depth() == 0 {
                continue;
            }
            let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
            if let Ok(rel) = entry.path().strip_prefix(root) {
                out.push((rel.to_path_buf(), is_dir));
            }
            if out.len() >= max {
                break;
            }
        }
        out
    }

    fn watch(&self, root: &Path, changes: Sender<Change>) -> Option<Watch> {
        use notify::Watcher;
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res
                && !event.kind.is_access()
            {
                let paths: Vec<PathBuf> = event
                    .paths
                    .into_iter()
                    .filter(|p| !p.components().any(|c| c.as_os_str() == ".git"))
                    .collect();
                if !paths.is_empty() {
                    let _ = changes.send(Change { paths });
                }
            }
        })
        .ok()?;
        watcher.watch(root, notify::RecursiveMode::Recursive).ok()?;
        Some(Watch(Box::new(watcher)))
    }

    fn shell_command(&self, cwd: &Path) -> CommandSpec {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| {
            if cfg!(windows) {
                "powershell.exe".into()
            } else {
                "/bin/zsh".into()
            }
        });
        CommandSpec {
            program: shell.into(),
            args: if cfg!(windows) {
                vec![]
            } else {
                vec!["-l".into()]
            },
            cwd: Some(cwd.to_path_buf()),
            env: Vec::new(),
        }
    }

    fn program_command(&self, spec: &ProgramSpec) -> CommandSpec {
        let program = if spec.program.contains('/') {
            PathBuf::from(&spec.program)
        } else {
            find_program(&spec.program).unwrap_or_else(|| spec.program.clone().into())
        };
        if spec.unset.is_empty() {
            return CommandSpec { program, args: spec.args.clone(), cwd: Some(spec.cwd.clone()), env: spec.env.clone() };
        }
        // `env -u` works the same for a plain process and a pseudo-terminal.
        let mut args: Vec<String> = spec.unset.iter().flat_map(|v| ["-u".to_string(), v.clone()]).collect();
        args.push(program.to_string_lossy().to_string());
        args.extend(spec.args.iter().cloned());
        CommandSpec { program: "/usr/bin/env".into(), args, cwd: Some(spec.cwd.clone()), env: spec.env.clone() }
    }
}

/// Another machine, reached with the system `ssh` (keys and `~/.ssh/config` as usual). Commands
/// share one control connection so each operation does not pay for a new handshake.
#[derive(Clone, Debug)]
pub struct SshHost {
    /// What `ssh` connects to: `host`, `user@host` or a `~/.ssh/config` alias.
    pub destination: String,
    /// The remote home folder, asked once on connect.
    home: Option<PathBuf>,
}

impl SshHost {
    /// Connect (one round trip to learn the remote home folder).
    pub fn connect(destination: &str) -> io::Result<Self> {
        let mut host = Self {
            destination: destination.to_string(),
            home: None,
        };
        let out = host.run("printf %s \"$HOME\"", None)?;
        host.home = Some(PathBuf::from(String::from_utf8_lossy(&out).to_string()));
        Ok(host)
    }

    fn control_args(&self) -> Vec<String> {
        let socket = std::env::temp_dir().join("workbench-ssh-%C");
        vec![
            "-o".into(),
            "ControlMaster=auto".into(),
            "-o".into(),
            format!("ControlPath={}", socket.display()),
            "-o".into(),
            "ControlPersist=600".into(),
        ]
    }

    /// The `ssh` invocation running `script` on the remote (`tty` asks for a terminal).
    pub fn ssh_spec(&self, script: &str, tty: bool) -> CommandSpec {
        let mut args = self.control_args();
        args.push(if tty { "-t".into() } else { "-T".into() });
        args.push(self.destination.clone());
        args.push(script.to_string());
        CommandSpec {
            program: "ssh".into(),
            args,
            cwd: None,
            env: Vec::new(),
        }
    }

    fn run(&self, script: &str, stdin: Option<&[u8]>) -> io::Result<Vec<u8>> {
        use std::io::Write;
        let mut cmd = self.ssh_spec(script, false).to_command();
        cmd.stdin(if stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn()?;
        if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(data)?;
        }
        let out = child.wait_with_output()?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(io::Error::other(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ))
        }
    }
}

/// Quote a string for a POSIX shell.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn q(path: &Path) -> String {
    shell_quote(&path.to_string_lossy())
}

impl Host for SshHost {
    fn label(&self) -> String {
        self.destination.clone()
    }

    fn is_remote(&self) -> bool {
        true
    }

    fn home_dir(&self) -> Option<PathBuf> {
        self.home.clone()
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.run(&format!("cat {}", q(path)), None)
    }

    fn read_from(&self, path: &Path, offset: u64) -> io::Result<Vec<u8>> {
        self.run(&format!("tail -c +{} {}", offset + 1, q(path)), None)
    }

    fn len(&self, path: &Path) -> io::Result<u64> {
        let out = self.run(&format!("wc -c < {}", q(path)), None)?;
        String::from_utf8_lossy(&out)
            .trim()
            .parse()
            .map_err(|_| io::Error::other("no length"))
    }

    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        self.run(&format!("cat > {}", q(path)), Some(contents))
            .map(|_| ())
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.run(&format!("mkdir -p {}", q(path)), None).map(|_| ())
    }

    fn exists(&self, path: &Path) -> bool {
        self.run(&format!("test -e {}", q(path)), None).is_ok()
    }

    fn is_file(&self, path: &Path) -> bool {
        self.run(&format!("test -f {}", q(path)), None).is_ok()
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        // `ls -1Lp`: one per line, symlinks followed, folders end in `/`.
        let out = self.run(&format!("ls -1ALp {}", q(path)), None)?;
        Ok(String::from_utf8_lossy(&out)
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| match l.strip_suffix('/') {
                Some(name) => DirEntry {
                    name: name.into(),
                    is_dir: true,
                },
                None => DirEntry {
                    name: l.into(),
                    is_dir: false,
                },
            })
            .collect())
    }

    fn list_files(&self, root: &Path, max: usize) -> Vec<(PathBuf, bool)> {
        // Git knows what is ignored; outside a repository fall back to find.
        let script = format!(
            "cd {} && (git ls-files -co --exclude-standard 2>/dev/null || find . -type f | sed 's|^\\./||') | head -n {max}",
            q(root)
        );
        let Ok(out) = self.run(&script, None) else {
            return Vec::new();
        };
        files_with_folders(String::from_utf8_lossy(&out).lines())
    }

    fn watch(&self, _root: &Path, _changes: Sender<Change>) -> Option<Watch> {
        None
    }

    fn shell_command(&self, cwd: &Path) -> CommandSpec {
        self.ssh_spec(&format!("cd {} && exec \"$SHELL\" -l", q(cwd)), true)
    }

    fn program_command(&self, spec: &ProgramSpec) -> CommandSpec {
        let args: Vec<String> = spec.args.iter().map(|a| shell_quote(a)).collect();
        let env: Vec<String> = spec.env.iter().map(|(k, v)| shell_quote(&format!("{k}={v}"))).collect();
        let unset: Vec<String> = spec.unset.iter().map(|v| format!("-u {}", shell_quote(v))).collect();
        let env = if env.is_empty() && unset.is_empty() {
            String::new()
        } else {
            format!("env {} ", unset.into_iter().chain(env).collect::<Vec<_>>().join(" "))
        };
        // A login shell, so the remote PATH finds the program.
        let inner = format!("cd {} && exec {env}{} {}", q(&spec.cwd), shell_quote(&spec.program), args.join(" "));
        self.ssh_spec(
            &format!("exec \"$SHELL\" -lc {}", shell_quote(&inner)),
            false,
        )
    }
}

/// Find a program on PATH or where installers put it (a window started from the Finder does
/// not get the shell's PATH).
pub fn find_program(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    let mut dirs: Vec<PathBuf> =
        std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".npm-global/bin"));
        dirs.push(home.join(format!(".{name}/local")));
    }
    dirs.push("/opt/homebrew/bin".into());
    dirs.push("/usr/local/bin".into());
    dirs.into_iter().map(|d| d.join(&exe)).find(|p| p.is_file())
}

/// Whether a process on this machine still exists (asks `ps`; sends no signal).
pub fn process_alive(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.is_empty())
}

/// Turn a list of file paths into files plus the folders that hold them, sorted.
pub fn files_with_folders<'a>(files: impl Iterator<Item = &'a str>) -> Vec<(PathBuf, bool)> {
    let mut set = std::collections::BTreeMap::new();
    for f in files.filter(|f| !f.is_empty()) {
        let path = PathBuf::from(f);
        for ancestor in path.ancestors().skip(1) {
            if ancestor.as_os_str().is_empty() {
                break;
            }
            set.insert(ancestor.to_path_buf(), true);
        }
        set.insert(path, false);
    }
    set.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branches() {
        assert_eq!(
            branch_from_head("ref: refs/heads/main\n").as_deref(),
            Some("main")
        );
        assert_eq!(
            branch_from_head("ref: refs/heads/feat/x").as_deref(),
            Some("feat/x")
        );
        assert_eq!(
            branch_from_head("0123456789abcdef").as_deref(),
            Some("0123456")
        );
    }

    #[test]
    fn local_program_and_shell_commands() {
        let host = LocalHost;
        let spec = ProgramSpec {
            program: "/bin/agent".into(),
            args: vec!["-p".into()],
            env: vec![("A".into(), "1".into())],
            unset: Vec::new(),
            cwd: "/p".into(),
        };
        let cmd = host.program_command(&spec);
        assert_eq!(cmd.program, PathBuf::from("/bin/agent"));
        assert_eq!(cmd.cwd.as_deref(), Some(Path::new("/p")));
        assert_eq!(cmd.args, ["-p"]);
        assert_eq!(cmd.env, [("A".to_string(), "1".to_string())]);
        assert_eq!(host.shell_command(Path::new("/p")).cwd.as_deref(), Some(Path::new("/p")));
        let cmd = host.program_command(&ProgramSpec { unset: vec!["X".into()], ..spec });
        assert_eq!(cmd.program, PathBuf::from("/usr/bin/env"));
        assert_eq!(cmd.args, ["-u", "X", "/bin/agent", "-p"]);
    }

    #[test]
    fn ssh_commands_quote_paths() {
        let host = SshHost { destination: "dev".into(), home: None };
        let shell = host.shell_command(Path::new("/srv/my app"));
        assert_eq!(shell.program, PathBuf::from("ssh"));
        assert!(shell.args.contains(&"-t".to_string()));
        assert_eq!(shell.args.last().unwrap(), "cd '/srv/my app' && exec \"$SHELL\" -l");
        let agent = host.program_command(&ProgramSpec {
            program: "agent".into(),
            args: vec!["--mode".into(), "it's".into()],
            env: vec![("K".into(), "v".into())],
            unset: vec!["X".into()],
            cwd: "/srv/it's".into(),
        });
        assert!(agent.args.contains(&"-T".to_string()));
        let script = agent.args.last().unwrap();
        assert!(script.starts_with("exec \"$SHELL\" -lc '"));
        assert!(script.contains("agent"));
        assert!(script.contains("env -u"));
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn folders_from_file_list() {
        let list = files_with_folders(["src/a.rs", "src/b/c.rs", "README.md"].into_iter());
        let names: Vec<_> = list
            .iter()
            .map(|(p, d)| (p.to_string_lossy().to_string(), *d))
            .collect();
        assert_eq!(
            names,
            [
                ("README.md".into(), false),
                ("src".into(), true),
                ("src/a.rs".into(), false),
                ("src/b".into(), true),
                ("src/b/c.rs".into(), false)
            ]
        );
    }

    #[test]
    fn local_watch_reports_a_write() {
        let dir = std::env::temp_dir().join(format!("wb-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let _watch = LocalHost.watch(&dir, tx).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::write(dir.join("x.txt"), "1").unwrap();
        let change = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(change.paths.iter().any(|p| p.ends_with("x.txt")));
        let _ = std::fs::remove_dir_all(dir);
    }
}
