//! Workbench: skills, code, Claude and a terminal for one project folder.
//!
//! `workbench [folder]` or `workbench --ssh host:/path`.

extern crate gpui_kit as gpui;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
use workbench_core::host::{Host, LocalHost, SshHost};

mod actions;
mod claude_panel;
mod claude_session;
mod editor_area;
mod file_tree;
mod palette;
mod perf;
mod pty;
mod skills_panel;
mod tab_strip;
mod terminal_panel;
mod terminal_view;
mod workspace;

use actions::*;

pub static START: OnceLock<Instant> = OnceLock::new();

/// Where to open: a host and a folder on it.
fn parse_args() -> anyhow::Result<(Arc<dyn Host>, PathBuf)> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(pos) = args.iter().position(|a| a == "--ssh") {
        let target = args
            .get(pos + 1)
            .ok_or_else(|| anyhow::anyhow!("--ssh needs host:/path"))?;
        let (dest, path) = target
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("--ssh needs host:/path"))?;
        let host = SshHost::connect(dest)?;
        return Ok((Arc::new(host), PathBuf::from(path)));
    }
    let folder = match args.first() {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir()?,
    };
    let folder = folder
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("{}: {e}", folder.display()))?;
    Ok((Arc::new(LocalHost), folder))
}

fn open_workspace(host: Arc<dyn Host>, root: PathBuf, cx: &mut App) {
    let title = format!(
        "{} - Workbench",
        root.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    );
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(1500.), px(940.)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(10.), px(8.))),
        }),
        app_owns_titlebar_drag: true,
        window_min_size: Some(size(px(640.), px(400.))),
        ..Default::default()
    };
    let result = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| workspace::Workspace::new(host, root, window, cx))
    });
    if let Err(e) = result {
        eprintln!("workbench: could not open the window: {e}");
    }
}

fn main() {
    START.get_or_init(Instant::now);
    let (host, root) = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("workbench: {e}");
            std::process::exit(2);
        }
    };
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            bind_keys(cx);
            cx.set_menus(menus());
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.on_action(|_: &OpenFolder, cx| {
                let rx = cx.prompt_for_paths(PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: Some("Open".into()),
                });
                cx.spawn(async move |cx: &mut AsyncApp| {
                    if let Ok(Ok(Some(paths))) = rx.await
                        && let Some(path) = paths.into_iter().next()
                    {
                        cx.update(|cx| open_workspace(Arc::new(LocalHost), path, cx));
                    }
                })
                .detach();
            });
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            open_workspace(host, root, cx);
            cx.activate(true);
        });
}
