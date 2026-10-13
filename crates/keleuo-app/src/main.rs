//! Keleuo: Claude, code, skills and terminals for one project folder.
//!
//! `keleuo [folder]` or `keleuo --ssh host:/path`: the directory browser opens at that
//! folder (the current one when none is given), listing each folder's agent sessions.
//! `--new` starts a session there directly. When a window is already open the request goes to
//! it (see `instance`) instead of opening a second one.

extern crate gpui_kit as gpui;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Instant;


use gpui_kit::*;
use keleuo_core::host::{Host, LocalHost, SshHost};
use keleuo_core::settings::Settings;

mod about;
mod actions;
mod app_icon;
mod attention;
mod browser;
mod backends;
mod changes_panel;
mod chat_panel;
mod closing;
mod agent_session;
mod agent_side;
mod control;
mod doc_panel;
mod dock_skin;
mod file_tree;
mod hooks;
mod icons;
mod palette;
mod panels;
mod perf;
mod procstats;
mod instance;
mod pty;
mod sheet_tabs;
mod settings_window;
mod shell;
mod skills_panel;
mod terminal_view;
mod theme;
mod web_panel;
mod workspace;

use actions::*;

pub static START: OnceLock<Instant> = OnceLock::new();

/// Recent folders and the remembered layout, shared by every window.
pub struct AppSettings(pub Rc<RefCell<Settings>>);

impl Global for AppSettings {}

/// The on-by-default behaviour from Settings (the defaults where there are no settings, as in
/// tests).
pub fn prefs(cx: &App) -> keleuo_core::settings::Prefs {
    cx.try_global::<AppSettings>().map(|s| s.0.borrow().prefs.clone()).unwrap_or_default()
}

/// The command line as a request: which folder, on which machine, browse or start.
fn parse_args() -> anyhow::Result<(instance::Request, Option<String>)> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let new = args.iter().any(|a| a == "--new");
    if args.iter().any(|a| a == "--quit") {
        return Ok((instance::Request { ssh: String::new(), path: PathBuf::new(), new: false, quit: true }, None));
    }
    if let Some(pos) = args.iter().position(|a| a == "--ssh") {
        let target = args.get(pos + 1).ok_or_else(|| anyhow::anyhow!("--ssh needs host:/path"))?;
        let (dest, path) = target.split_once(':').ok_or_else(|| anyhow::anyhow!("--ssh needs host:/path"))?;
        return Ok((instance::Request { ssh: dest.into(), path: PathBuf::from(path), new, quit: false }, None));
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let Some(arg) = args.iter().find(|a| !a.starts_with("--")) else {
        // Like `code .`: the folder the command was run from. Started from the Finder (the
        // root folder), the browser opens at the last folder instead.
        let path = if cwd == Path::new("/") { PathBuf::new() } else { cwd };
        return Ok((instance::Request { ssh: String::new(), path, new, quit: false }, None));
    };
    match cwd.join(arg).canonicalize() {
        Ok(p) if p.is_dir() => Ok((instance::Request { ssh: String::new(), path: p, new, quit: false }, None)),
        _ => Ok((
            instance::Request { ssh: String::new(), path: PathBuf::new(), new: false, quit: false },
            Some(format!("{arg} is not a folder that exists.")),
        )),
    }
}

/// Quit, asking first when a session is working (see `Shell::confirm_quit`).
fn request_quit(cx: &mut App) {
    let target = cx.global::<control::ControlTargets>().workspace.clone();
    let asked = target.is_some_and(|(handle, shell)| {
        handle
            .update(cx, |_, window, cx| shell.update(cx, |s, cx| s.confirm_quit(window, cx)).is_ok())
            .unwrap_or(false)
    });
    if !asked {
        quit_now(cx);
    }
}

/// Quit now: the window's socket and hook settings go first.
pub fn quit_now(cx: &mut App) {
    instance::stop_listening();
    hooks::cleanup(cx);
    cx.quit();
}

/// Carry out a request in a window: open one, or use the open one.
fn carry_out(request: instance::Request, note: Option<String>, cx: &mut App) {
    let instance::Request { ssh, path, new, quit } = request;
    if quit {
        request_quit(cx);
        return;
    }
    if ssh.is_empty() {
        let path = if path.as_os_str().is_empty() { shell::default_folder(cx) } else { path };
        show(Arc::new(LocalHost), path, new, note, cx);
        return;
    }
    // Connecting is a network round trip: off the UI thread.
    cx.spawn(async move |cx: &mut AsyncApp| {
        let dest = ssh.clone();
        let host = cx.background_spawn(async move { SshHost::connect(&dest) }).await;
        cx.update(|cx| match host {
            Ok(host) => show(Arc::new(host), path, new, note, cx),
            Err(e) => {
                let message = format!("Could not connect to {ssh}: {e}");
                eprintln!("keleuo: {message}");
                tell(message, cx);
            }
        });
    })
    .detach();
}

/// A notice in the open window, if there is one.
fn tell(message: String, cx: &mut App) {
    if let Some((handle, _)) = cx.global::<control::ControlTargets>().workspace.clone() {
        let _ = handle.update(cx, |_, window, cx| gpui_kit::component::WindowExt::push_notification(window, message, cx));
    }
}

fn show(host: Arc<dyn Host>, path: PathBuf, new: bool, note: Option<String>, cx: &mut App) {
    let existing = cx.global::<control::ControlTargets>().workspace.clone();
    if let Some((handle, shell)) = existing
        && shell.upgrade().is_some()
    {
        let _ = handle.update(cx, |_, window, cx| {
            let _ = shell.update(cx, |s, cx| {
                if new {
                    let backend = backends::backend(None, cx);
                    s.new_session_in(host, path, backend, window, cx);
                } else {
                    s.open_browser(host, path, note, window, cx);
                }
            });
            if !background() {
                window.activate_window();
            }
        });
        return;
    }
    let start = if new { shell::Start::Session(host, path) } else { shell::Start::Browse(host, path, note) };
    open_window(start, cx);
}

/// Run in the background (tests): windows open without taking keyboard focus and the app
/// does not activate itself.
/// Turn one kind of alert on or off (see `attention`); the menu's check and the Dock badge
/// follow.
fn toggle_notify(cx: &mut App, which: fn(&mut keleuo_core::settings::Notify) -> &mut bool) {
    cx.update_global::<AppSettings, _>(|settings, _| {
        let mut s = settings.0.borrow_mut();
        let on = which(&mut s.notify);
        *on = !*on;
        let _ = s.save();
    });
    cx.set_menus(menus(cx));
}

fn background() -> bool {
    keleuo_core::env_var("BACKGROUND").is_ok_and(|v| v != "0")
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

fn open_window(start: shell::Start, cx: &mut App) {
    let options = window_options("Keleuo".into(), 1500., 940., cx);
    match gpui_kit::open_window(options, cx, |window, cx| {
        // The system went light or dark: follow it (unless the setting says otherwise).
        window.observe_window_appearance(|_, cx| theme::sync(cx)).detach();
        cx.new(|cx| shell::Shell::new(start, window, cx))
    }) {
        Ok((handle, shell)) => {
            cx.global_mut::<control::ControlTargets>().workspace = Some((handle, shell.downgrade()));
        }
        Err(e) => eprintln!("keleuo: could not open the window: {e}"),
    }
}

fn main() {
    START.get_or_init(Instant::now);
    let (request, note) = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("keleuo: {e}");
            std::process::exit(2);
        }
    };
    // A window is already open: it takes the request.
    if note.is_none() && perf::step("hand over", || instance::hand_over(&request)) {
        return;
    }
    // `--quit` with no window that takes it (none open, or one too old to know it).
    if request.quit {
        std::process::exit(1);
    }
    gpui_kit::application()
        .with_assets(icons::AppAssets)
        .run(move |cx: &mut App| {
            perf::step("gpui init", || gpui_kit::init(cx));
            let settings = perf::step("settings", || Settings::load(Settings::default_path()));
            let forced = match settings.appearance.as_str() {
                "light" => Some(WindowAppearance::Light),
                "dark" => Some(WindowAppearance::Dark),
                _ => None,
            };
            cx.set_global(AppSettings(Rc::new(RefCell::new(settings))));
            cx.set_window_appearance(forced);
            cx.set_global(panels::Names(Rc::new(RefCell::new(
                keleuo_core::tabs::NameStore::open_default(),
            ))));
            cx.set_global(panels::Renaming::default());
            cx.set_global(backends::Backends(backends::registry()));
            workspace::register_panels(cx);
            theme::apply(theme::wants_dark(cx), cx);
            cx.on_action(|_: &ShowAbout, cx| about::open(cx));
            cx.on_action(|_: &ShowSettings, cx| settings_window::open(cx));
            cx.on_action(|_: &AppearanceSystem, cx| theme::set_appearance("system", cx));
            cx.on_action(|_: &AppearanceLight, cx| theme::set_appearance("light", cx));
            cx.on_action(|_: &AppearanceDark, cx| theme::set_appearance("dark", cx));
            control::start(cx);
            hooks::init(cx);
            app_icon::set();
            // Cached images and kept terminals older than 30 days go (off the UI thread).
            std::thread::spawn(|| {
                if let Some(root) = keleuo_core::images::root() {
                    keleuo_core::images::clean_old(&root, std::time::SystemTime::now());
                    keleuo_core::term_history::clean_old(std::time::SystemTime::now());
                }
            });
            bind_keys(cx);
            cx.set_menus(menus(cx));
            cx.on_action(|_: &ToggleNotifyWaiting, cx| toggle_notify(cx, |n| &mut n.waiting));
            cx.on_action(|_: &ToggleNotifyFinished, cx| toggle_notify(cx, |n| &mut n.finished));
            cx.on_action(|_: &ToggleCodeFont, cx| {
                cx.update_global::<AppSettings, _>(|settings, _| {
                    let mut s = settings.0.borrow_mut();
                    s.always_proportional = !s.always_proportional;
                    let _ = s.save();
                });
                cx.set_menus(menus(cx));
                cx.refresh_windows();
            });
            cx.on_action(|_: &Quit, cx| request_quit(cx));
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    instance::stop_listening();
                    hooks::cleanup(cx);
                    cx.quit();
                }
            })
            .detach();
            // Later `keleuo` commands come here instead of opening another app.
            let (tx, rx) = async_channel::unbounded::<instance::Request>();
            instance::listen(move |r| {
                let _ = tx.send_blocking(r);
            });
            cx.spawn(async move |cx: &mut AsyncApp| {
                while let Ok(r) = rx.recv().await {
                    cx.update(|cx| carry_out(r, None, cx));
                }
            })
            .detach();
            perf::step("open the window", || carry_out(request, note, cx));
            if !background() {
                cx.activate(true);
            }
        });
}
