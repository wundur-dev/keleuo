//! A quick symbol index: definitions found by line patterns per language (fn, struct, class,
//! def, ...). The first step toward code navigation: go to definition by name and a file
//! outline, before tree-sitter queries and LSP.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::files::language_for;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: &'static str,
    pub path: PathBuf,
    /// 1-based.
    pub line: u32,
    /// 0-based column of the name.
    pub column: u32,
}

/// Definitions in one file's text.
pub fn definitions(path: &Path, text: &str) -> Vec<Symbol> {
    let lang = language_for(path);
    let rules: &[(&str, &'static str)] = match lang {
        "rust" => &[
            ("fn ", "function"),
            ("struct ", "struct"),
            ("enum ", "enum"),
            ("trait ", "trait"),
            ("type ", "type"),
            ("mod ", "module"),
            ("const ", "const"),
            ("static ", "static"),
            ("macro_rules! ", "macro"),
            ("union ", "union"),
        ],
        "python" => &[
            ("def ", "function"),
            ("class ", "class"),
            ("async def ", "function"),
        ],
        "javascript" | "typescript" | "tsx" => &[
            ("function ", "function"),
            ("function* ", "function"),
            ("class ", "class"),
            ("interface ", "interface"),
            ("type ", "type"),
            ("enum ", "enum"),
            ("const ", "const"),
            ("let ", "variable"),
        ],
        "go" => &[("func ", "function"), ("type ", "type")],
        "csharp" | "java" | "kotlin" | "scala" | "swift" => &[
            ("class ", "class"),
            ("interface ", "interface"),
            ("struct ", "struct"),
            ("enum ", "enum"),
            ("record ", "record"),
            ("object ", "object"),
            ("fun ", "function"),
            ("func ", "function"),
            ("def ", "function"),
            ("protocol ", "protocol"),
        ],
        "ruby" => &[
            ("def ", "function"),
            ("class ", "class"),
            ("module ", "module"),
        ],
        "lua" => &[("function ", "function"), ("local function ", "function")],
        "elixir" => &[
            ("def ", "function"),
            ("defp ", "function"),
            ("defmodule ", "module"),
        ],
        "markdown" => &[("# ", "heading"), ("## ", "heading"), ("### ", "heading")],
        _ => &[],
    };
    if rules.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let indent = raw.len() - raw.trim_start().len();
        let mut line = raw.trim_start();
        if lang == "markdown" {
            if let Some((prefix, kind)) = rules.iter().find(|(p, _)| line.starts_with(p)) {
                let name = line[prefix.len()..].trim().to_string();
                if !name.is_empty() {
                    out.push(Symbol {
                        name,
                        kind,
                        path: path.to_path_buf(),
                        line: i as u32 + 1,
                        column: (indent + prefix.len()) as u32,
                    });
                }
            }
            continue;
        }
        // Strip modifiers that may come first.
        let mut offset = indent;
        loop {
            let before = line;
            for m in [
                "pub(crate) ",
                "pub(super) ",
                "pub ",
                "export default ",
                "export ",
                "default ",
                "async ",
                "unsafe ",
                "extern \"C\" ",
                "public ",
                "private ",
                "protected ",
                "internal ",
                "static ",
                "abstract ",
                "final ",
                "sealed ",
                "partial ",
                "override ",
                "virtual ",
                "open ",
                "data ",
                "inline ",
                "declare ",
                "readonly ",
            ] {
                if let Some(rest) = line.strip_prefix(m) {
                    offset += m.len();
                    line = rest;
                }
            }
            if line == before {
                break;
            }
        }
        let Some((prefix, kind)) = rules.iter().find(|(p, _)| line.starts_with(p)) else {
            continue;
        };
        let rest = &line[prefix.len()..];
        // Go methods: `func (r *T) Name(`.
        let (rest, skipped) = if lang == "go" && rest.starts_with('(') {
            match rest.find(')') {
                Some(close) => {
                    let after = &rest[close + 1..];
                    let trimmed = after.trim_start();
                    (trimmed, close + 1 + after.len() - trimmed.len())
                }
                None => (rest, 0),
            }
        } else {
            (rest, 0)
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        if name.is_empty()
            || (*kind == "variable" || *kind == "const") && indent > 0 && lang != "rust"
        {
            continue;
        }
        out.push(Symbol {
            name,
            kind,
            path: path.to_path_buf(),
            line: i as u32 + 1,
            column: (offset + prefix.len() + skipped) as u32,
        });
    }
    out
}

/// Definitions across the project, by name.
#[derive(Clone, Debug, Default)]
pub struct SymbolIndex {
    by_name: HashMap<String, Vec<Symbol>>,
    count: usize,
}

impl SymbolIndex {
    pub fn add(&mut self, symbols: Vec<Symbol>) {
        for s in symbols {
            self.count += 1;
            self.by_name.entry(s.name.clone()).or_default().push(s);
        }
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Where `name` is defined; definitions in `prefer` (the current file) come first.
    pub fn lookup(&self, name: &str, prefer: Option<&Path>) -> Vec<Symbol> {
        let mut found = self.by_name.get(name).cloned().unwrap_or_default();
        found.sort_by_key(|s| (Some(s.path.as_path()) != prefer, s.path.clone(), s.line));
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_definitions() {
        let text = "pub struct Foo {\n}\nimpl Foo {\n    pub(crate) async fn bar(&self) {}\n}\nfn main() {}\nmacro_rules! m { }\n";
        let defs = definitions(Path::new("a.rs"), text);
        let got: Vec<_> = defs
            .iter()
            .map(|s| (s.name.as_str(), s.kind, s.line, s.column))
            .collect();
        assert_eq!(
            got,
            [
                ("Foo", "struct", 1, 11),
                ("bar", "function", 4, 24),
                ("main", "function", 6, 3),
                ("m", "macro", 7, 13)
            ]
        );
    }

    #[test]
    fn python_go_ts() {
        let py = definitions(
            Path::new("g.py"),
            "class A:\n    def greet(self):\n        x = 1\n",
        );
        assert_eq!(
            py.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["A", "greet"]
        );
        let go = definitions(
            Path::new("m.go"),
            "func (s *Server) Start(ctx) {}\ntype Server struct{}\n",
        );
        assert_eq!(go[0].name, "Start");
        assert_eq!(go[0].column, 17);
        let ts = definitions(
            Path::new("x.ts"),
            "export default class App {}\nexport const route = 1;\nfunction f() { const inner = 2; }\n",
        );
        assert_eq!(
            ts.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["App", "route", "f"]
        );
    }

    #[test]
    fn index_prefers_current_file() {
        let mut idx = SymbolIndex::default();
        idx.add(definitions(Path::new("/p/b.rs"), "fn run() {}\n"));
        idx.add(definitions(Path::new("/p/a.rs"), "fn run() {}\n"));
        assert_eq!(idx.lookup("run", None)[0].path, PathBuf::from("/p/a.rs"));
        assert_eq!(
            idx.lookup("run", Some(Path::new("/p/b.rs")))[0].path,
            PathBuf::from("/p/b.rs")
        );
        assert!(idx.lookup("nope", None).is_empty());
        assert_eq!(idx.len(), 2);
    }
}
