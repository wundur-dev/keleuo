//! Closing something with work in it asks first, like any Mac app: a file with unsaved changes
//! (Save, Don't Save, Cancel), a session in the middle of a turn, a terminal running a command.
//! With nothing to lose it just closes.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::*;
use keleuo_core::host::Host;

use crate::doc_panel::DocPanel;

/// Ask in a native dialog over `window`; the index of the answer, `None` when the window went.
pub async fn ask(
    window: AnyWindowHandle,
    level: PromptLevel,
    message: String,
    detail: String,
    answers: Vec<PromptButton>,
    cx: &mut AsyncApp,
) -> Option<usize> {
    let answer = window.update(cx, |_, window, cx| window.prompt(level, &message, Some(&detail), &answers, cx)).ok()?;
    answer.await.ok()
}

/// A file to write: its panel, where it goes, and the text.
type SaveJob = (Entity<DocPanel>, Arc<dyn Host>, PathBuf, String);

/// Write the files' unsaved text; each one written is marked saved. What could not be written.
async fn save(docs: Vec<Entity<DocPanel>>, cx: &mut AsyncApp) -> Vec<String> {
    let jobs: Vec<SaveJob> = cx
        .update(|cx| docs.iter().filter_map(|d| d.read(cx).unsaved(cx).map(|(h, p, t)| (d.clone(), h, p, t))).collect::<Vec<_>>());
    let mut failed = Vec::new();
    for (doc, host, path, text) in jobs {
        let (p, t) = (path.clone(), text.clone());
        match cx.background_spawn(async move { host.write(&p, t.as_bytes()) }).await {
            Ok(()) => {
                cx.update(|cx| doc.update(cx, |d, cx| d.mark_saved(text, cx)));
            }
            Err(e) => failed.push(format!("{}: {e}", path.display())),
        }
    }
    failed
}

/// "Save the changes?" for these files. True to go on closing (saved, or let go), false to stop.
pub async fn save_or_drop(docs: Vec<Entity<DocPanel>>, window: AnyWindowHandle, cx: &mut AsyncApp) -> bool {
    if docs.is_empty() {
        return true;
    }
    let names: Vec<String> = cx.update(|cx| docs.iter().map(|d| d.read(cx).auto_name()).collect::<Vec<_>>());
    let (message, detail, save_label) = match names.as_slice() {
        [one] => (
            format!("Do you want to save the changes you made to \u{201c}{one}\u{201d}?"),
            "Your changes will be lost if you don't save them.".to_string(),
            "Save",
        ),
        many => (
            format!("{} files have unsaved changes. Save them?", many.len()),
            format!("{}\n\nChanges you don't save will be lost.", many.join("\n")),
            "Save All",
        ),
    };
    let answers = vec![
        PromptButton::Ok(save_label.into()),
        PromptButton::Other("Don't Save".into()),
        PromptButton::Cancel("Cancel".into()),
    ];
    match ask(window, PromptLevel::Warning, message, detail, answers, cx).await {
        Some(0) => {
            let failed = save(docs, cx).await;
            if failed.is_empty() {
                return true;
            }
            let ok = vec![PromptButton::Ok("OK".into())];
            let _ = ask(window, PromptLevel::Critical, "Some files could not be saved.".into(), failed.join("\n"), ok, cx).await;
            false
        }
        Some(1) => true,
        _ => false,
    }
}

/// Close a tab with `close`; a file with unsaved changes asks first.
pub fn close_tab(view: AnyView, close: impl FnOnce(&mut Window, &mut App) + 'static, window: &mut Window, cx: &mut App) {
    let Some(doc) = view.downcast::<DocPanel>().ok().filter(|d| d.read(cx).dirty) else {
        close(window, cx);
        return;
    };
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        if save_or_drop(vec![doc], handle, cx).await {
            let _ = handle.update(cx, |_, window, cx| close(window, cx));
        }
    })
    .detach();
}

/// "N sessions are working and M terminals are running a command", as much as applies.
pub fn busy_summary(sessions: usize, commands: usize) -> String {
    let plural = |n: usize, one: &str, many: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
    let s = plural(sessions, "session is working", "sessions are working");
    let t = plural(commands, "terminal is running a command", "terminals are running a command");
    match (sessions, commands) {
        (_, 0) => s,
        (0, _) => t,
        _ => format!("{s} and {t}"),
    }
}

#[cfg(test)]
mod tests {
    use super::busy_summary;

    #[test]
    fn says_what_is_busy() {
        assert_eq!(busy_summary(1, 0), "1 session is working");
        assert_eq!(busy_summary(2, 1), "2 sessions are working and 1 terminal is running a command");
        assert_eq!(busy_summary(0, 3), "3 terminals are running a command");
    }
}
