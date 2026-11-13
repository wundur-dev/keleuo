//! The game assets being made in this window, by folder: their records, what is being made for
//! each now, what went wrong, and which try is picked. The chat's asset cards and the asset
//! tabs both draw from here and act through it, so they always agree; a change redraws every
//! window (see `revision`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui_kit::*;
use wundur_core::assets::{self, Asset, Make, View};

/// One asset as the window holds it.
#[derive(Clone, Default)]
pub struct Entry {
    pub asset: Asset,
    /// The project it is in (the session's folder).
    pub project: PathBuf,
    /// What is being made now: several at once (the three views).
    pub busy: Vec<Job>,
    pub error: Option<String>,
    /// The concept picked to change or approve.
    pub picked: Option<String>,
}

/// What is being made for an asset: one of each at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Job {
    Concept,
    View(View),
    Model,
}

impl Job {
    pub fn of(what: &Make) -> Self {
        match what {
            Make::Concept { .. } | Make::Change { .. } => Job::Concept,
            Make::View(v) => Job::View(*v),
            Make::Model => Job::Model,
        }
    }
}

#[derive(Default)]
struct Store {
    entries: HashMap<PathBuf, Entry>,
    revision: u64,
}

impl Global for Store {}

/// Changes so far: a view drawing assets redraws (and remeasures) when it changes.
pub fn revision(cx: &App) -> u64 {
    cx.try_global::<Store>().map_or(0, |s| s.revision)
}

/// The project an asset's folder is in: `<project>/assets/<kind>s/<name>`.
pub fn project_of(dir: &Path) -> PathBuf {
    dir.ancestors().nth(3).map(Path::to_path_buf).unwrap_or_default()
}

/// The asset in `dir`, read from its record the first time.
pub fn entry(dir: &Path, cx: &mut App) -> Option<Entry> {
    let store = cx.default_global::<Store>();
    if !store.entries.contains_key(dir) {
        let asset = assets::load(dir)?;
        let picked = asset.concept.clone().or_else(|| asset.concepts.last().map(|a| a.file.clone()));
        store.entries.insert(dir.to_path_buf(), Entry { asset, project: project_of(dir), picked, ..Default::default() });
    }
    store.entries.get(dir).cloned()
}

/// Change the asset in `dir` (saving its record when `save`), and redraw.
fn update(dir: &Path, save: bool, cx: &mut App, f: impl FnOnce(&mut Entry)) {
    if entry(dir, cx).is_none() {
        return;
    }
    let store = cx.global_mut::<Store>();
    if let Some(e) = store.entries.get_mut(dir) {
        f(e);
        if save && let Err(err) = assets::save(dir, &e.asset) {
            e.error = Some(format!("Couldn't save the asset: {err}"));
        }
    }
    store.revision += 1;
    cx.refresh_windows();
}

/// A new asset in `project`: its folder (another name's when that one is taken) and record,
/// remembering the chat it was started in.
pub fn create(project: &Path, name: &str, kind: &str, brief: &str, origin: Option<assets::Origin>, cx: &mut App) -> Result<PathBuf, String> {
    let (name, brief) = (name.trim(), brief.trim());
    if name.is_empty() || brief.is_empty() {
        return Err("Give it a name and say what it is.".into());
    }
    let mut dir = assets::folder_for(project, kind, name);
    let mut n = 2;
    while dir.join(assets::RECORD).exists() {
        dir = assets::folder_for(project, kind, &format!("{name} {n}"));
        n += 1;
    }
    let asset = Asset { name: name.into(), kind: kind.into(), brief: brief.into(), origin, ..Default::default() };
    assets::save(&dir, &asset).map_err(|e| format!("Couldn't save the asset: {e}"))?;
    let store = cx.default_global::<Store>();
    store.entries.insert(dir.clone(), Entry { asset, project: project.to_path_buf(), ..Default::default() });
    store.revision += 1;
    cx.refresh_windows();
    Ok(dir)
}

pub fn pick(dir: &Path, file: &str, cx: &mut App) {
    let file = file.to_string();
    update(dir, false, cx, |e| e.picked = Some(file));
}

/// Approve the picked concept: on to the views.
pub fn approve_concept(dir: &Path, cx: &mut App) {
    update(dir, true, cx, |e| {
        if let Some(f) = e.picked.clone() {
            e.asset.approve_concept(&f);
        }
    });
}

pub fn approve_view(dir: &Path, view: View, file: &str, cx: &mut App) {
    let file = file.to_string();
    update(dir, true, cx, |e| e.asset.approve_view(view, &file));
}

pub fn approve_model(dir: &Path, file: &str, cx: &mut App) {
    let file = file.to_string();
    update(dir, true, cx, |e| e.asset.approve_model(&file));
}

/// The 3D viewer's page for the model `file` of the asset in `dir` (made by the pack's
/// view_model, here, with no network), then `then` with the page to open.
pub fn view_3d(dir: &Path, file: &str, cx: &mut App, then: impl FnOnce(PathBuf, &mut App) + 'static) {
    let Some(e) = entry(dir, cx) else { return };
    let Some(pack) = pack() else { return };
    let Some(tool) = pack.manifest.tools.iter().find(|t| t.name == "view_model").cloned() else { return };
    let project = e.project.clone();
    let model = dir.join(file).strip_prefix(&project).map(Path::to_path_buf).unwrap_or_else(|_| dir.join(file));
    let title = e.asset.name.clone();
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let none = |_: &str| None;
        let settings = std::collections::BTreeMap::new();
        let grant = wundur_core::packs::Grant { cwd: &project, secret: &none, settings: &settings, extra_hosts: Vec::new(), allow_http: false };
        let out = wundur_core::packs::run(&pack, &tool, &serde_json::json!({ "model": model.to_string_lossy(), "title": title }), &grant);
        let _ = tx.send_blocking(out);
    });
    let dir = dir.to_path_buf();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let Ok(out) = rx.recv().await else { return };
        cx.update(|cx| match out.show {
            Some(page) if !out.failed => then(page, cx),
            _ => update(&dir, false, cx, |e| e.error = Some(format!("Couldn't make the viewer: {}", out.text))),
        });
    })
    .detach();
}

/// The game-assets pack, when installed.
fn pack() -> Option<wundur_core::packs::Pack> {
    let dir = wundur_core::packs::dir()?;
    wundur_core::packs::installed(&dir).into_iter().find(|p| p.manifest.id == assets::PACK)
}

/// Make `what` for the asset in `dir`, off the UI thread, through the game-assets pack with the
/// person's keys (several kinds at once; one of each). `then` runs when it's done (on the UI
/// thread), with whether it worked.
pub fn make(dir: &Path, what: Make, cx: &mut App, then: impl FnOnce(bool, &mut App) + 'static) {
    let Some(e) = entry(dir, cx) else { return };
    let job = Job::of(&what);
    if e.busy.contains(&job) {
        return;
    }
    let Some(pack) = pack() else {
        update(dir, false, cx, |e| e.error = Some("Install the Game assets pack (Settings > Tools) and add your OpenAI or Azure key.".into()));
        return;
    };
    let settings = cx.global::<crate::AppSettings>().0.borrow().pack_settings.get(assets::PACK).cloned().unwrap_or_default();
    let (project, dir, mut asset) = (e.project.clone(), dir.to_path_buf(), e.asset.clone());
    update(&dir, false, cx, |e| {
        e.busy.push(job);
        e.error = None;
    });
    // Test copies (WUNDUR_TEST_BACKENDS) may use a stand-in service on this machine, with a
    // stand-in key (WUNDUR_TEST_PACK_KEY), never the person's.
    let test = wundur_core::env_var("TEST_BACKENDS").is_ok();
    let test_key = test.then(|| wundur_core::env_var("TEST_PACK_KEY").ok()).flatten();
    let (tx, rx) = async_channel::bounded(1);
    let worker_dir = dir.clone();
    std::thread::spawn(move || {
        let secret = |name: &str| match &test_key {
            Some(k) => Some(k.clone()),
            None => wundur_core::secrets::get(&pack.secret_account(name)),
        };
        let (extra_hosts, allow_http) = if test { (vec!["127.0.0.1".to_string()], true) } else { (Vec::new(), false) };
        let grant = wundur_core::packs::Grant { cwd: &project, secret: &secret, settings: &settings, extra_hosts, allow_http };
        let made = assets::make(&worker_dir, &mut asset, &what, &pack, &grant);
        let _ = tx.send_blocking(made);
    });
    cx.spawn(async move |cx: &mut AsyncApp| {
        let Ok(made) = rx.recv().await else { return };
        cx.update(|cx| {
            let ok = made.is_ok();
            update(&dir, ok, cx, |e| {
                e.busy.retain(|j| *j != job);
                match made {
                    // Added to the record as it is now: others may have come back meanwhile.
                    Ok(attempt) => {
                        if job == Job::Concept {
                            e.picked = Some(attempt.file.clone());
                        }
                        e.asset.record(attempt);
                    }
                    Err(why) => e.error = Some(why),
                }
            });
            then(ok, cx);
        });
    })
    .detach();
}

/// The views not made yet, all at once (each its own request).
pub fn make_views(dir: &Path, cx: &mut App) {
    let Some(e) = entry(dir, cx) else { return };
    for view in View::ALL.into_iter().filter(|v| !e.asset.views.iter().any(|a| a.view == Some(*v))) {
        make(dir, Make::View(view), cx, |_, _| {});
    }
}

/// The assets in `project` started in the chat of session `id`: their folders and the turn
/// each was started in.
pub fn started_in(project: &Path, id: &str) -> Vec<(PathBuf, Option<String>)> {
    let mut found = Vec::new();
    let Ok(kinds) = std::fs::read_dir(project.join("assets")) else { return found };
    for kind in kinds.flatten() {
        let Ok(names) = std::fs::read_dir(kind.path()) else { continue };
        for dir in names.flatten().map(|d| d.path()) {
            if let Some(a) = assets::load(&dir)
                && let Some(o) = &a.origin
                && o.session == id
            {
                found.push((dir, o.turn.clone()));
            }
        }
    }
    found.sort();
    found
}
