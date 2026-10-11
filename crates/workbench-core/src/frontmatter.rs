//! The YAML frontmatter at the top of SKILL.md, agent and command files.

/// The fields Workbench shows, plus the markdown body after the header.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    /// The text after the closing `---` (or the whole file when there is no header).
    pub body: String,
}

/// Split a file into its frontmatter and body and read `name` and `description`.
///
/// Real files are not always valid YAML (an unquoted colon in a description is common), so a
/// failed YAML parse falls back to reading simple `key: value` lines.
pub fn parse(text: &str) -> Frontmatter {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some((header, body)) = split(text) else {
        return Frontmatter {
            body: text.to_string(),
            ..Default::default()
        };
    };
    let (name, description) = match serde_yaml::from_str::<serde_yaml::Value>(header) {
        Ok(serde_yaml::Value::Mapping(map)) => (
            map.get("name").and_then(scalar),
            map.get("description").and_then(scalar),
        ),
        _ => (
            line_value(header, "name"),
            line_value(header, "description"),
        ),
    };
    Frontmatter {
        name,
        description: description.map(|d| d.split_whitespace().collect::<Vec<_>>().join(" ")),
        body: body.to_string(),
    }
}

/// `(header, body)` when the text starts with a `---` line and has a closing one.
fn split(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("---")?;
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            let header = &rest[..offset];
            let body = &rest[offset + line.len()..];
            return Some((header, body.trim_start_matches(['\r', '\n'])));
        }
        offset += line.len();
    }
    None
}

fn scalar(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::String(s) => Some(s.trim().to_string()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
    .filter(|s| !s.is_empty())
}

fn line_value(header: &str, key: &str) -> Option<String> {
    header
        .lines()
        .find_map(|line| {
            let (k, v) = line.split_once(':')?;
            (k.trim() == key).then(|| v.trim().trim_matches(['"', '\'']).to_string())
        })
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_name_description_and_body() {
        let fm = parse("---\nname: tdd\ndescription: \"Write the test first\"\n---\n\n# Body\n");
        assert_eq!(fm.name.as_deref(), Some("tdd"));
        assert_eq!(fm.description.as_deref(), Some("Write the test first"));
        assert_eq!(fm.body, "# Body\n");
    }

    #[test]
    fn folded_description_becomes_one_line() {
        let fm = parse("---\nname: x\ndescription: >\n  first line\n  second line\n---\nbody");
        assert_eq!(fm.description.as_deref(), Some("first line second line"));
    }

    #[test]
    fn invalid_yaml_falls_back_to_lines() {
        let fm = parse("---\nname: x\ndescription: Use when: things break\n---\n");
        assert_eq!(fm.name.as_deref(), Some("x"));
        assert_eq!(fm.description.as_deref(), Some("Use when: things break"));
    }

    #[test]
    fn no_header_is_all_body() {
        let fm = parse("# Just markdown\n");
        assert_eq!(fm.name, None);
        assert_eq!(fm.body, "# Just markdown\n");
    }

    #[test]
    fn crlf_header() {
        let fm = parse("---\r\nname: win\r\n---\r\nbody");
        assert_eq!(fm.name.as_deref(), Some("win"));
        assert_eq!(fm.body, "body");
    }
}
