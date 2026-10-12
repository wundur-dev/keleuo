//! The agent backends this window offers. This is the only file that names a backend; the
//! rest of the app works with `keleuo_core::agent::{Backend, Agent, Event}` alone (checked by
//! the test below).

use std::sync::Arc;

use gpui_kit::{App, Global};
use keleuo_core::agent::{Backend, Registry};
use keleuo_core::backends::claude::ClaudeBackend;
use keleuo_core::backends::scripted::ScriptedBackend;

pub struct Backends(pub Registry);

impl Global for Backends {}

/// Claude Code; plus the scripted test agent when `KELEUO_TEST_BACKENDS=1`.
pub fn registry() -> Registry {
    let mut all: Vec<Arc<dyn Backend>> = vec![Arc::new(ClaudeBackend)];
    if keleuo_core::env_var("TEST_BACKENDS").is_ok_and(|v| v == "1") {
        all.push(Arc::new(ScriptedBackend { step: std::time::Duration::from_millis(120) }));
    }
    Registry::new(all)
}

/// The backend saved as `id`, or the default when it is not available.
pub fn backend(id: Option<&str>, cx: &App) -> Arc<dyn Backend> {
    let reg = &cx.global::<Backends>().0;
    id.and_then(|id| reg.get(id))
        .or_else(|| reg.default_backend())
        .expect("at least one backend is registered")
}

pub fn all(cx: &App) -> Vec<Arc<dyn Backend>> {
    cx.global::<Backends>().0.all().to_vec()
}

#[cfg(test)]
mod tests {
    /// Nothing outside this file names a particular backend.
    #[test]
    fn only_the_registry_names_a_backend() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let forbidden = ["ClaudeCode", "ClaudeBackend", "claude::", "ScriptedBackend", "scripted::"];
        let mut bad = Vec::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            // `._*` are macOS metadata files some drives keep beside the real ones, not source.
            let skip = path.file_name().is_some_and(|n| n == "backends.rs" || n.to_string_lossy().starts_with("._"));
            if skip || path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for (n, line) in text.lines().enumerate() {
                if forbidden.iter().any(|f| line.contains(f)) {
                    bad.push(format!("{}:{}: {line}", path.display(), n + 1));
                }
            }
        }
        assert!(bad.is_empty(), "backend named outside backends.rs:\n{}", bad.join("\n"));
    }
}
