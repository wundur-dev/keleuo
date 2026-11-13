//! A workflow: an agent harness's script running many agents in the background (Claude Code's
//! Workflow tool), shown as one background agent. Its result names where it keeps its journal
//! (a line when each of its agents starts, one when each returns), which says how far it is.

/// Where a launched workflow records its agents (`journal.jsonl` in it), from its result.
pub fn dir(result: &str) -> Option<std::path::PathBuf> {
    result.lines().find_map(|l| Some(std::path::PathBuf::from(l.trim().strip_prefix("Transcript dir:")?.trim())))
}

/// How far a workflow is, from its `journal.jsonl` (a line when each of its agents starts, one
/// when each returns): `(returned, started)`.
pub fn progress(journal: &str) -> (usize, usize) {
    let (mut done, mut started) = (0, 0);
    for line in journal.lines() {
        if line.contains("\"type\":\"started\"") {
            started += 1;
        } else if line.contains("\"type\":\"result\"") {
            done += 1;
        }
    }
    (done, started)
}

