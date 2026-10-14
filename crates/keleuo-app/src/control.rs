//! A debug command channel for tests and scripting, so the window can be driven without
//! synthetic keyboard or mouse input. Off unless `KELEUO_CONTROL` names a FIFO (or a plain
//! file to tail); each line is a command for the newest workspace window (see
//! `Shell::command` and `Workspace::command`); `key <keystrokes>` types inside the window.
//! Replies go to stderr as `control: <line> -> <result>`.

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;

use gpui_kit::*;

use crate::shell::Shell;

/// Where control commands go.
#[derive(Default)]
pub struct ControlTargets {
    pub workspace: Option<(AnyWindowHandle, WeakEntity<Shell>)>,
}

impl Global for ControlTargets {}

pub fn start(cx: &mut App) {
    cx.set_global(ControlTargets::default());
    let Some(path) = keleuo_core::env_var_os("CONTROL").map(PathBuf::from) else {
        return;
    };
    let (tx, rx) = async_channel::unbounded::<String>();
    std::thread::spawn(move || {
        // How far a plain file has been read: only lines added since are new (a FIFO starts
        // empty each time it is opened). A file that shrank was started over.
        let mut read = 0u64;
        loop {
            // Opening a FIFO blocks until a writer appears; EOF means the writer left.
            let Ok(mut file) = std::fs::File::open(&path) else {
                std::thread::sleep(std::time::Duration::from_millis(200));
                continue;
            };
            let plain = file.metadata().ok().filter(|m| m.is_file()).map(|m| m.len());
            if let Some(len) = plain {
                if len < read {
                    read = 0;
                }
                let _ = file.seek(SeekFrom::Start(read));
            }
            let mut reader = BufReader::new(file);
            let mut line = String::new();
            // Whole lines only: one still being written is read next time.
            while reader.read_line(&mut line).is_ok_and(|n| n > 0) && line.ends_with('\n') {
                if plain.is_some() {
                    read += line.len() as u64;
                }
                let command = line.trim().to_string();
                line.clear();
                if !command.is_empty() && tx.send_blocking(command).is_err() {
                    return;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok(line) = rx.recv().await {
            let reply = cx.update(|cx| dispatch(&line, cx));
            eprintln!("control: {line} -> {reply}");
        }
    })
    .detach();
}

fn dispatch(line: &str, cx: &mut App) -> String {
    let targets = cx.global::<ControlTargets>();
    let workspace = targets.workspace.clone();
    let Some((handle, ws)) = workspace else {
        return "no workspace".into();
    };
    if let Some(keys) = line.strip_prefix("key ") {
        // Keystrokes dispatched inside the app (no system input): `key cmd-n`.
        let keys = keys.trim().to_string();
        return handle
            .update(cx, |_, window, cx| {
                let mut handled = Vec::new();
                for k in keys.split_whitespace() {
                    match Keystroke::parse(k) {
                        Ok(ks) => {
                            handled.push(format!("{k}:{}", window.dispatch_keystroke(ks, cx)))
                        }
                        Err(e) => handled.push(format!("{k}:bad ({e})")),
                    }
                }
                handled.join(" ")
            })
            .unwrap_or_else(|_| "window closed".into());
    }
    handle
        .update(cx, |_, window, cx| {
            ws.update(cx, |shell, cx| {
                if let Some(reply) = shell.command(line, window, cx) {
                    return reply;
                }
                match shell.active_workspace() {
                    Some(w) => w.update(cx, |w, cx| w.command(line, window, cx)),
                    None => "no session".into(),
                }
            })
            .unwrap_or_else(|_| "window closed".into())
        })
        .unwrap_or_else(|_| "window closed".into())
}
