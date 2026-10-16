//! The spelling checker where the system has none to offer (Linux and the other Unixes): the
//! Hunspell dictionary installed for the user's language, read by a checker in Rust. Without a
//! dictionary, spelling is simply off. Words taught with Learn Spelling are kept in a word
//! list in Keleuo's config folder; Ignore Spelling lasts as long as the message box.

use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use spellbook::Dictionary;

/// Where distributions put Hunspell dictionaries.
const SYSTEM_DIRS: [&str; 3] = ["/usr/share/hunspell", "/usr/share/myspell/dicts", "/usr/share/myspell"];

/// The language used when the user's has no dictionary.
const FALLBACK: &str = "en_US";

/// The dictionary, once read (on a thread of its own: a large one takes a moment); `None` in
/// it when there is none.
static DICTIONARY: OnceLock<RwLock<Option<Dictionary>>> = OnceLock::new();

/// One message box's checker. They share the dictionary and the learned words.
#[derive(Default)]
pub struct Checker {
    ignored: HashSet<String>,
}

impl Checker {
    /// A checker; the first one starts reading the dictionary.
    pub fn new() -> Self {
        static STARTED: OnceLock<()> = OnceLock::new();
        STARTED.get_or_init(|| {
            std::thread::spawn(|| {
                let found = locate(language().as_deref(), &SYSTEM_DIRS.map(PathBuf::from)).and_then(|(aff, dic)| open(&aff, &dic));
                let found = found.map(|mut dictionary| {
                    for word in learned(learned_path().as_deref()) {
                        let _ = dictionary.add(&word);
                    }
                    dictionary
                });
                let _ = DICTIONARY.set(RwLock::new(found));
            });
        });
        Self::default()
    }

    /// The misspelled words of `text`, as byte ranges; `None` while the dictionary is being
    /// read, nothing when there is none.
    pub fn misspelled(&self, text: &str) -> Option<Vec<Range<usize>>> {
        let dictionary = DICTIONARY.get()?.read().ok()?;
        Some(dictionary.as_ref().map_or_else(Vec::new, |d| misspelled(d, &self.ignored, text)))
    }

    /// What `word` may have been meant to be, the likeliest first.
    pub fn guesses(&self, word: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(Some(dictionary)) = DICTIONARY.get().and_then(|d| d.read().ok()).as_deref() {
            dictionary.suggest(word, &mut out);
        }
        out
    }

    /// Learn Spelling: `word` is a word from now on, in every message box and the next runs.
    pub fn learn(&mut self, word: &str) {
        if let Some(path) = learned_path() {
            let mut words = learned(Some(&path));
            words.push(word.to_string());
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, words.join("\n") + "\n");
        }
        if let Some(Ok(mut dictionary)) = DICTIONARY.get().map(RwLock::write)
            && let Some(dictionary) = dictionary.as_mut()
        {
            let _ = dictionary.add(word);
        }
    }

    /// Ignore Spelling: `word` is not flagged in this message box.
    pub fn ignore(&mut self, word: &str) {
        self.ignored.insert(word.to_string());
    }
}

/// The misspelled words of `text` that are not among `ignored`.
fn misspelled(dictionary: &Dictionary, ignored: &HashSet<String>, text: &str) -> Vec<Range<usize>> {
    super::words(text, &[])
        .into_iter()
        .filter(|w| !ignored.contains(&text[w.clone()]) && !dictionary.check(&text[w.clone()]))
        .collect()
}

/// The user's language from the environment (`de_DE` from `LANG=de_DE.UTF-8`).
fn language() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.is_empty())
        .map(|v| v.split(['.', '@']).next().unwrap_or_default().to_string())
        .filter(|v| v != "C" && v != "POSIX")
}

/// The `.aff` and `.dic` files for `language` in the first of `dirs` that has them: its exact
/// name, then any dictionary of the same language (`de_AT` for `de`), then [`FALLBACK`].
fn locate(language: Option<&str>, dirs: &[PathBuf]) -> Option<(PathBuf, PathBuf)> {
    let pair = |dir: &Path, name: &str| {
        let (aff, dic) = (dir.join(format!("{name}.aff")), dir.join(format!("{name}.dic")));
        (aff.is_file() && dic.is_file()).then_some((aff, dic))
    };
    let base = language.map(|l| l.split(['_', '-']).next().unwrap_or(l).to_string());
    let same_language = |dir: &Path, base: &str| {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok()?.file_name().into_string().ok()?.strip_suffix(".dic").map(str::to_string))
            .filter(|n| n == base || n.starts_with(&format!("{base}_")))
            .collect();
        names.sort();
        names.iter().find_map(|n| pair(dir, n))
    };
    language
        .and_then(|l| dirs.iter().find_map(|d| pair(d, l)))
        .or_else(|| base.as_deref().and_then(|b| dirs.iter().find_map(|d| same_language(d, b))))
        .or_else(|| dirs.iter().find_map(|d| pair(d, FALLBACK)))
}

/// A dictionary read from its two files. Older dictionaries are in a single-byte encoding
/// rather than UTF-8; those are read as Latin-1.
fn open(aff: &Path, dic: &Path) -> Option<Dictionary> {
    let text = |path: &Path| {
        let bytes = std::fs::read(path).ok()?;
        Some(String::from_utf8(bytes).unwrap_or_else(|e| e.into_bytes().into_iter().map(char::from).collect()))
    };
    Dictionary::new(&text(aff)?, &text(dic)?).ok()
}

/// The word list Learn Spelling adds to.
fn learned_path() -> Option<PathBuf> {
    crate::settings::config_dir().map(|d| d.join("spelling-words.txt"))
}

/// The learned words, one a line.
fn learned(path: Option<&Path>) -> Vec<String> {
    path.and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| t.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const AFF: &str = "SET UTF-8\nTRY esianrtolcdugmphbyfvkwzESIANRTOLCDUGMPHBYFVKWZ\n\nSFX S Y 1\nSFX S 0 s .\n";
    const DIC: &str = "4\nhello\nworld\ncat/S\nspell\n";

    #[test]
    fn checks_with_a_dictionary() {
        let dictionary = Dictionary::new(AFF, DIC).unwrap();
        let text = "hello wrld, cats and `cta`";
        let found: Vec<&str> = misspelled(&dictionary, &HashSet::new(), text).into_iter().map(|r| &text[r]).collect();
        assert_eq!(found, ["wrld", "and"]);
        let ignored = HashSet::from(["and".to_string()]);
        assert_eq!(misspelled(&dictionary, &ignored, text).len(), 1);
        let mut guesses = Vec::new();
        dictionary.suggest("wrld", &mut guesses);
        assert!(guesses.contains(&"world".to_string()));
    }

    #[test]
    fn finds_the_users_dictionary_or_the_fallback() {
        let dir = std::env::temp_dir().join(format!("keleuo-hunspell-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["de_AT", "en_US"] {
            std::fs::write(dir.join(format!("{name}.aff")), AFF).unwrap();
            std::fs::write(dir.join(format!("{name}.dic")), DIC).unwrap();
        }
        let dirs = [dir.join("missing"), dir.clone()];
        let name = |l: Option<&str>| locate(l, &dirs).map(|(aff, _)| aff.file_stem().unwrap().to_string_lossy().into_owned());
        assert_eq!(name(Some("de_AT")).as_deref(), Some("de_AT"));
        assert_eq!(name(Some("de_DE")).as_deref(), Some("de_AT"));
        assert_eq!(name(Some("fr_FR")).as_deref(), Some("en_US"));
        assert_eq!(name(None).as_deref(), Some("en_US"));
        assert!(open(&dir.join("de_AT.aff"), &dir.join("de_AT.dic")).is_some_and(|d| d.check("cats")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
