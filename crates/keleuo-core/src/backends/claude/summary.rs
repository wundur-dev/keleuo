//! The one-line summary shown on a collapsed tool card.

use serde_json::Value;

/// A short description of a tool call: the command for Bash, the file for Read or Edit, ...
/// Paths under `cwd` are shown relative to it.
pub fn tool_summary(name: &str, input: &Value, cwd: &str) -> String {
    let get = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or_default();
    let path = |k: &str| relative(get(k), cwd);
    let text = match name {
        "Bash" => get("command").to_string(),
        "Read" | "Edit" | "MultiEdit" | "Write" => path("file_path"),
        "NotebookEdit" => path("notebook_path"),
        "Glob" => get("pattern").to_string(),
        "Grep" => {
            let p = get("path");
            if p.is_empty() {
                get("pattern").to_string()
            } else {
                format!("{} in {}", get("pattern"), relative(p, cwd))
            }
        }
        "WebFetch" => get("url").to_string(),
        "WebSearch" => get("query").to_string(),
        "Task" | "Agent" => get("description").to_string(),
        "Skill" => get("skill").to_string(),
        "TodoWrite" => {
            let n = input
                .get("todos")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            format!("{n} items")
        }
        _ => input
            .as_object()
            .and_then(|o| o.values().find_map(Value::as_str))
            .unwrap_or_default()
            .to_string(),
    };
    one_line(&text, 120)
}

pub use crate::conversation::one_line;

fn relative(path: &str, cwd: &str) -> String {
    if cwd.is_empty() {
        return path.to_string();
    }
    path.strip_prefix(cwd)
        .map(|rest| rest.trim_start_matches('/').to_string())
        .filter(|rest| !rest.is_empty())
        .unwrap_or_else(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summaries() {
        assert_eq!(
            tool_summary("Bash", &json!({"command": "ls -la"}), "/p"),
            "ls -la"
        );
        assert_eq!(
            tool_summary("Edit", &json!({"file_path": "/p/src/a.rs"}), "/p"),
            "src/a.rs"
        );
        assert_eq!(
            tool_summary("Read", &json!({"file_path": "/q/a.rs"}), "/p"),
            "/q/a.rs"
        );
        assert_eq!(
            tool_summary("Grep", &json!({"pattern": "fn", "path": "/p/src"}), "/p"),
            "fn in src"
        );
        assert_eq!(
            tool_summary("Bash", &json!({"command": "a\nb"}), ""),
            "a ..."
        );
        assert_eq!(tool_summary("Mystery", &json!({"x": "y"}), ""), "y");
        assert_eq!(one_line(&"x".repeat(10), 4), "xxxx...");
    }
}
