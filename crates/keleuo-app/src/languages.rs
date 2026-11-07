//! Highlighting for languages gpui-component parses but doesn't colour: it registers C#,
//! Swift and CMake with an empty highlight query (gpui-component 0.7.1, `languages.rs`), so
//! their files open as plain text. Each grammar's own crate ships the query; registered again
//! here with it, once, at start.

use gpui_kit::component::highlighter::{LanguageConfig, LanguageRegistry};

/// The languages, by the names gpui-component knows them by, with their highlight queries.
fn extra() -> Vec<LanguageConfig> {
    vec![
        LanguageConfig::new("csharp", tree_sitter_c_sharp::LANGUAGE.into(), vec![], tree_sitter_c_sharp::HIGHLIGHTS_QUERY, "", ""),
        LanguageConfig::new("swift", tree_sitter_swift::LANGUAGE.into(), vec![], tree_sitter_swift::HIGHLIGHTS_QUERY, "", tree_sitter_swift::LOCALS_QUERY),
        LanguageConfig::new("cmake", tree_sitter_cmake::LANGUAGE.into(), vec![], tree_sitter_cmake::HIGHLIGHTS_QUERY, "", ""),
    ]
}

/// Register them (after gpui-component's own, which they replace).
pub fn register() {
    let registry = LanguageRegistry::singleton();
    for config in extra() {
        registry.register(&config.name.clone(), &config);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csharp_swift_and_cmake_get_their_highlight_queries() {
        register();
        for name in ["csharp", "cs", "swift", "cmake"] {
            let config = LanguageRegistry::singleton().language(name).unwrap_or_else(|| panic!("{name} not registered"));
            assert!(!config.highlights.is_empty(), "{name} has no highlight query");
            // The query must compile against its grammar, or nothing is coloured.
            tree_sitter::Query::new(&config.language.clone().unwrap(), &config.highlights).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
}
