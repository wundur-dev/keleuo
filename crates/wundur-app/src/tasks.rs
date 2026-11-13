//! Tasks for the person: lists of things to do that an agent hands over (its `add_tasks` tool,
//! through `wundur-mcp`), drawn as cards the person works through and ticks off, instead of a
//! wall of text in the chat. Each list belongs to a session's folder and says which session
//! made it; an agent can tick off a task it checked itself (`complete_task`) and see what is
//! still open (`list_tasks`). Kept in the settings folder (`tasks.json`), so they outlast a
//! restart.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Task {
    pub id: u64,
    pub title: String,
    /// A sentence or two: what, and why it matters.
    pub detail: String,
    /// A command to run for it, shown with a copy button.
    pub command: Option<String>,
    /// A page to open for it.
    pub link: Option<String>,
    pub done: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskList {
    pub id: u64,
    pub title: String,
    /// The folder of the session that made it.
    pub folder: PathBuf,
    /// That session's name, then.
    pub from: String,
    /// Seconds since the epoch.
    pub made: u64,
    pub tasks: Vec<Task>,
}

impl TaskList {
    pub fn open(&self) -> usize {
        self.tasks.iter().filter(|t| !t.done).count()
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Tasks {
    next: u64,
    /// Newest first.
    pub lists: Vec<TaskList>,
}

impl Global for Tasks {}

fn file() -> Option<PathBuf> {
    wundur_core::settings::config_dir().map(|d| d.join("tasks.json"))
}

fn store(cx: &mut App) -> &mut Tasks {
    if !cx.has_global::<Tasks>() {
        let loaded = file().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| serde_json::from_str::<Tasks>(&t).ok()).unwrap_or_default();
        cx.set_global(loaded);
    }
    cx.global_mut::<Tasks>()
}

fn save(cx: &mut App) {
    let text = serde_json::to_string_pretty(store(cx)).unwrap_or_default();
    if let Some(f) = file() {
        let _ = std::fs::write(f, text);
    }
    cx.refresh_windows();
}

/// A new list from the session `from` in `folder`: its id and its tasks' ids.
pub fn add_list(title: String, folder: PathBuf, from: String, tasks: Vec<Task>, cx: &mut App) -> (u64, Vec<u64>) {
    let s = store(cx);
    s.next += 1;
    let list_id = s.next;
    let mut ids = Vec::new();
    let tasks = tasks
        .into_iter()
        .map(|mut t| {
            s.next += 1;
            t.id = s.next;
            t.done = false;
            ids.push(t.id);
            t
        })
        .collect();
    let made = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    s.lists.insert(0, TaskList { id: list_id, title, folder, from, made, tasks });
    save(cx);
    (list_id, ids)
}

pub fn lists(cx: &mut App) -> Vec<TaskList> {
    store(cx).lists.clone()
}

/// Open tasks in every list: the toolbar's count.
pub fn open_count(cx: &mut App) -> usize {
    store(cx).lists.iter().map(TaskList::open).sum()
}

/// Tick task `id` off (or back on); false when there is no such task.
pub fn set_done(id: u64, done: bool, cx: &mut App) -> bool {
    let found = store(cx).lists.iter_mut().flat_map(|l| l.tasks.iter_mut()).find(|t| t.id == id).map(|t| t.done = done).is_some();
    if found {
        save(cx);
    }
    found
}

/// Drop list `id` (all its tasks).
pub fn remove_list(id: u64, cx: &mut App) {
    store(cx).lists.retain(|l| l.id != id);
    save(cx);
}

/// The lists made in `folder`, as an agent reads them: each task with its id and whether done.
pub fn describe(folder: &std::path::Path, cx: &mut App) -> String {
    let lists: Vec<TaskList> = store(cx).lists.iter().filter(|l| l.folder == folder).cloned().collect();
    if lists.is_empty() {
        return "No task lists for this folder.".into();
    }
    let mut out = String::new();
    for l in lists {
        out.push_str(&format!("{} ({} open of {}):\n", l.title, l.open(), l.tasks.len()));
        for t in &l.tasks {
            out.push_str(&format!("- [{}] {} (id {})\n", if t.done { "x" } else { " " }, t.title, t.id));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn a_list_counts_its_open_tasks() {
        let l = TaskList {
            tasks: vec![Task { title: "a".into(), ..Default::default() }, Task { title: "b".into(), done: true, ..Default::default() }],
            ..Default::default()
        };
        assert_eq!(l.open(), 1);
    }

    #[::core::prelude::v1::test]
    fn lists_read_back_from_their_file() {
        let t = Tasks {
            next: 3,
            lists: vec![TaskList { id: 1, title: "API chores".into(), tasks: vec![Task { id: 2, title: "Add the DNS record".into(), command: Some("az network dns record-set".into()), ..Default::default() }], ..Default::default() }],
        };
        let back: Tasks = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!((back.next, back.lists[0].tasks[0].command.as_deref()), (3, Some("az network dns record-set")));
    }
}
