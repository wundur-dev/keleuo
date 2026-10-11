//! A debug command channel for tests and scripting, so the window can be driven without
//! synthetic keyboard or mouse input. Off unless `WORKBENCH_CONTROL` names a FIFO (or a plain
//! file to tail); each line is a command for the newest workspace window (see
//! `Workspace::command`) or, while the folder picker is open, `pick <n>` / `pick <path>`.
//! Replies go to stderr as `control: <line> -> <result>`.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use gpui_kit::*;

use crate::picker::Picker;
use crate::workspace::Workspace;

/// Where control commands go.
#[derive(Default)]
pub struct ControlTargets {
    pub workspace: Option<(AnyWindowHandle, WeakEntity<Workspace>)>,
    pub picker: Option<(AnyWindowHandle, WeakEntity<Picker>)>,
}

impl Global for ControlTargets {}

pub fn start(cx: &mut App) {
    cx.set_global(ControlTargets::default());
    let Some(path) = std::env::var_os("WORKBENCH_CONTROL").map(PathBuf::from) else {
        return;
    };
    let (tx, rx) = async_channel::unbounded::<String>();
    std::thread::spawn(move || {
        loop {
            // Opening a FIFO blocks until a writer appears; EOF means the writer left.
            let Ok(file) = std::fs::File::open(&path) else {
                std::thread::sleep(std::time::Duration::from_millis(200));
                continue;
            };
            for line in BufReader::new(file).lines() {
                let Ok(line) = line else { break };
                if !line.trim().is_empty() && tx.send_blocking(line).is_err() {
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
    let picker = targets.picker.clone();
    let workspace = targets.workspace.clone();
    if let Some(arg) = line.strip_prefix("pick ") {
        let Some((handle, picker)) = picker else {
            return "no picker".into();
        };
        let arg = arg.trim().to_string();
        return handle
            .update(cx, |_, window, cx| {
                picker
                    .update(cx, |p, cx| {
                        let path = match arg.parse::<usize>() {
                            Ok(n) => p.recent.get(n.saturating_sub(1)).cloned(),
                            Err(_) => Some(PathBuf::from(&arg)),
                        };
                        match path {
                            Some(path) => {
                                p.choose(path, window, cx);
                                "ok".to_string()
                            }
                            None => "no such recent folder".to_string(),
                        }
                    })
                    .unwrap_or_else(|_| "picker closed".into())
            })
            .unwrap_or_else(|_| "picker closed".into());
    }
    let Some((handle, ws)) = workspace else {
        return "no workspace".into();
    };
    handle
        .update(cx, |_, window, cx| {
            ws.update(cx, |ws, cx| ws.command(line, window, cx))
                .unwrap_or_else(|_| "workspace closed".into())
        })
        .unwrap_or_else(|_| "window closed".into())
}
