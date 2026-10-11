//! Workbench: Claude, code, skills and terminals for one project folder.
//!
//! `workbench [folder]` or `workbench --ssh host:/path`. With no folder, or one that does not
//! exist, a folder picker with the recent folders opens instead.

extern crate gpui_kit as gpui;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
use workbench_core::host::{Host, LocalHost, SshHost};
use workbench_core::settings::Settings;

mod actions;
mod claude_panel;
mod claude_session;
mod control;
mod doc_panel;
mod file_tree;
mod palette;
mod panels;
mod perf;
mod picker;
mod pty;
mod sessions;
mod skills_panel;
mod terminal_view;
mod theme;
mod workspace;

use actions::*;

pub static START: OnceLock<Instant> = OnceLock::new();

/// Recent folders and the remembered layout, shared by every window.
pub struct AppSettings(pub Rc<RefCell<Settings>>);

impl Global for AppSettings {}

/// What to show first.
enum Start {
    Open(Arc<dyn Host>, PathBuf),
    /// The folder picker, with a note when a given path was not found.
    Pick(Option<String>),
}

fn parse_args() -> anyhow::Result<Start> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(pos) = args.iter().position(|a| a == "--ssh") {
        let target = args
            .get(pos + 1)
            .ok_or_else(|| anyhow::anyhow!("--ssh needs host:/path"))?;
        let (dest, path) = target
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("--ssh needs host:/path"))?;
        let host = SshHost::connect(dest)?;
        return Ok(Start::Open(Arc::new(host), PathBuf::from(path)));
    }
    let Some(arg) = args.first() else {
        return Ok(Start::Pick(None));
    };
    match PathBuf::from(arg).canonicalize() {
        Ok(p) if p.is_dir() => Ok(Start::Open(Arc::new(LocalHost), p)),
        _ => Ok(Start::Pick(Some(format!(
            "{arg} is not a folder that exists."
        )))),
    }
}

/// Run in the background (tests): windows open without taking keyboard focus and the app
/// does not activate itself.
fn background() -> bool {
    std::env::var("WORKBENCH_BACKGROUND").is_ok_and(|v| v != "0")
}

fn window_options(title: String, w: f32, h: f32, cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(w), px(h)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(10.), px(8.))),
        }),
        app_owns_titlebar_drag: true,
        focus: !background(),
        window_min_size: Some(size(px(640.), px(400.))),
        ..Default::default()
    }
}

pub fn open_workspace(host: Arc<dyn Host>, root: PathBuf, cx: &mut App) {
    if !host.is_remote() {
        let settings = cx.global::<AppSettings>().0.clone();
        settings.borrow_mut().add_recent(&root);
        let _ = settings.borrow().save();
    }
    let title = format!(
        "{} - Workbench",
        root.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    );
    let options = window_options(title, 1500., 940., cx);
    match gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| workspace::Workspace::new(host, root, window, cx))
    }) {
        Ok((handle, ws)) => {
            let targets = cx.global_mut::<control::ControlTargets>();
            targets.workspace = Some((handle, ws.downgrade()));
            targets.picker = None;
        }
        Err(e) => eprintln!("workbench: could not open the window: {e}"),
    }
}

fn open_picker(message: Option<String>, cx: &mut App) {
    let recent = cx.global::<AppSettings>().0.borrow().existing_recent();
    let options = window_options("Workbench".into(), 560., 520., cx);
    match gpui_kit::open_window(options, cx, |_, cx| {
        cx.new(|_| picker::Picker { message, recent })
    }) {
        Ok((handle, p)) => {
            cx.global_mut::<control::ControlTargets>().picker = Some((handle, p.downgrade()))
        }
        Err(e) => eprintln!("workbench: could not open the window: {e}"),
    }
}

fn main() {
    START.get_or_init(Instant::now);
    let start = match parse_args() {
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
            cx.set_global(AppSettings(Rc::new(RefCell::new(Settings::load(
                Settings::default_path(),
            )))));
            cx.set_global(panels::Names(Rc::new(RefCell::new(
                workbench_core::tabs::NameStore::open_default(),
            ))));
            cx.set_global(panels::Renaming::default());
            workspace::register_panels(cx);
            theme::apply(cx);
            control::start(cx);
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
            match start {
                Start::Open(host, root) => open_workspace(host, root, cx),
                Start::Pick(message) => open_picker(message, cx),
            }
            if !background() {
                cx.activate(true);
            }
        });
}
