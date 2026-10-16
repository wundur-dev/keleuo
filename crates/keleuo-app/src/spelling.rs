//! Spelling in the message box, from the system's own checker: NSSpellChecker on macOS, the
//! Spell Checking API on Windows, and elsewhere the Hunspell dictionary installed for the
//! user's language (see [`keleuo_core::spelling::hunspell`]). Which words are worth checking
//! is [`keleuo_core::spelling`]'s; this asks the checker about them, a line at a time, and
//! remembers the answers so an edit only asks about the lines it changed.
//!
//! The checkers are called on the main thread (NSSpellChecker must be), so a check asks about
//! a limited amount of text at once and says when there is more to do.

use std::collections::HashMap;
use std::ops::Range;

use keleuo_core::spelling::chunks;

/// The most text asked about in one check; what is left waits for the next. NSSpellChecker
/// takes around five milliseconds a kilobyte, so a check stays well inside a frame.
const BUDGET: usize = 2 * 1024;

/// A system's spelling checker, for one message box.
trait Source {
    /// The misspelled words of `text` (a line or less), as byte ranges. `None` while the
    /// checker is not ready yet.
    fn misspelled(&mut self, text: &str) -> Option<Vec<Range<usize>>>;
    /// What the word at `word` in `text` may have been meant to be, the likeliest first.
    fn guesses(&mut self, text: &str, word: Range<usize>) -> Vec<String>;
    /// Learn Spelling: a word from now on, everywhere.
    fn learn(&mut self, word: &str);
    /// Ignore Spelling: not flagged in this message box.
    fn ignore(&mut self, word: &str);
}

/// One message box's spelling: its checker, and what it found in each line asked about.
pub struct Spelling {
    source: Box<dyn Source>,
    found: HashMap<String, Vec<Range<usize>>>,
}

/// What a check found.
pub struct Checked {
    /// The misspelled words among those asked about.
    pub misspelled: Vec<Range<usize>>,
    /// False when lines were left for the next check (too much text at once, or a checker
    /// still getting ready).
    pub done: bool,
}

impl Spelling {
    pub fn new() -> Self {
        Self { source: system(), found: HashMap::new() }
    }

    /// Which of `words` (byte ranges of `text`, in order) are misspelled. Lines asked about
    /// before are answered from memory.
    pub fn check(&mut self, text: &str, words: &[Range<usize>]) -> Checked {
        let mut found = HashMap::new();
        let mut misspelled = Vec::new();
        let (mut asked, mut done) = (0, true);
        let mut words = words.iter().peekable();
        for chunk in chunks(text) {
            let mut inside = Vec::new();
            while let Some(w) = words.next_if(|w| w.start < chunk.end) {
                if w.start >= chunk.start && w.end <= chunk.end {
                    inside.push(w.clone());
                }
            }
            if inside.is_empty() {
                continue;
            }
            let line = &text[chunk.clone()];
            let known = self.found.remove(line).or_else(|| found.get(line).cloned());
            let wrong = match known {
                Some(wrong) => wrong,
                None if asked < BUDGET => match self.source.misspelled(line) {
                    Some(wrong) => {
                        asked += line.len();
                        wrong
                    }
                    None => {
                        done = false;
                        continue;
                    }
                },
                None => {
                    done = false;
                    continue;
                }
            };
            misspelled.extend(inside.into_iter().filter(|w| {
                wrong.iter().any(|r| chunk.start + r.start < w.end && w.start < chunk.start + r.end)
            }));
            found.insert(line.to_string(), wrong);
        }
        // Only the lines there are now are kept.
        self.found = found;
        Checked { misspelled, done }
    }

    /// Up to `n` guesses for the word at `word` in `text`.
    pub fn guesses(&mut self, text: &str, word: Range<usize>, n: usize) -> Vec<String> {
        let Some(line) = chunks(text).into_iter().find(|c| c.start <= word.start && word.end <= c.end) else {
            return Vec::new();
        };
        let mut guesses = self.source.guesses(&text[line.clone()], word.start - line.start..word.end - line.start);
        guesses.truncate(n);
        guesses
    }

    pub fn learn(&mut self, word: &str) {
        self.source.learn(word);
        self.found.clear();
    }

    pub fn ignore(&mut self, word: &str) {
        self.source.ignore(word);
        self.found.clear();
    }
}

#[cfg(target_os = "macos")]
fn system() -> Box<dyn Source> {
    Box::new(mac::Checker::new())
}

#[cfg(target_os = "windows")]
fn system() -> Box<dyn Source> {
    Box::new(win::Checker::new())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn system() -> Box<dyn Source> {
    Box::new(keleuo_core::spelling::hunspell::Checker::new())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl Source for keleuo_core::spelling::hunspell::Checker {
    fn misspelled(&mut self, text: &str) -> Option<Vec<Range<usize>>> {
        keleuo_core::spelling::hunspell::Checker::misspelled(self, text)
    }

    fn guesses(&mut self, text: &str, word: Range<usize>) -> Vec<String> {
        keleuo_core::spelling::hunspell::Checker::guesses(self, &text[word])
    }

    fn learn(&mut self, word: &str) {
        keleuo_core::spelling::hunspell::Checker::learn(self, word)
    }

    fn ignore(&mut self, word: &str) {
        keleuo_core::spelling::hunspell::Checker::ignore(self, word)
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ops::Range;

    use keleuo_core::spelling::{utf8_offset, utf16_offset};
    use objc2::rc::{Retained, autoreleasepool};
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::{NSRange, NSString};

    /// NSTextCheckingTypeSpelling: spelling only, no grammar.
    const SPELLING: u64 = 1 << 1;

    /// NSSpellChecker with a spell document of its own, so Ignore Spelling stays in its
    /// message box. The system finds the language of the text by itself, as text fields do.
    pub struct Checker {
        tag: isize,
    }

    fn shared() -> Retained<AnyObject> {
        // SAFETY: the shared checker always exists; called on the main thread.
        unsafe { msg_send![class!(NSSpellChecker), sharedSpellChecker] }
    }

    impl Checker {
        pub fn new() -> Self {
            // SAFETY: a class method returning a plain integer.
            let tag: isize = unsafe { msg_send![class!(NSSpellChecker), uniqueSpellDocumentTag] };
            Self { tag }
        }
    }

    impl Drop for Checker {
        fn drop(&mut self) {
            // SAFETY: closing the document this checker opened.
            unsafe {
                let _: () = msg_send![&*shared(), closeSpellDocumentWithTag: self.tag];
            }
        }
    }

    impl super::Source for Checker {
        fn misspelled(&mut self, text: &str) -> Option<Vec<Range<usize>>> {
            autoreleasepool(|_| {
                let string = NSString::from_str(text);
                let whole = NSRange::new(0, string.length());
                // SAFETY: on the main thread; checkString:range:types:options:… as documented,
                // with no options and no orthography or word count wanted back. The results
                // are NSTextCheckingResults, each with the range of a misspelled word.
                unsafe {
                    let results: Option<Retained<AnyObject>> = msg_send![
                        &*shared(),
                        checkString: &*string,
                        range: whole,
                        types: SPELLING,
                        options: std::ptr::null::<AnyObject>(),
                        inSpellDocumentWithTag: self.tag,
                        orthography: std::ptr::null_mut::<*mut AnyObject>(),
                        wordCount: std::ptr::null_mut::<isize>()
                    ];
                    let Some(results) = results else { return Some(Vec::new()) };
                    let count: usize = msg_send![&*results, count];
                    let found = (0..count)
                        .map(|i| {
                            let result: *mut AnyObject = msg_send![&*results, objectAtIndex: i];
                            let r: NSRange = msg_send![result, range];
                            utf8_offset(text, r.location)..utf8_offset(text, r.location + r.length)
                        })
                        .collect();
                    Some(found)
                }
            })
        }

        fn guesses(&mut self, text: &str, word: Range<usize>) -> Vec<String> {
            autoreleasepool(|_| {
                let string = NSString::from_str(text);
                let start = utf16_offset(text, word.start);
                let range = NSRange::new(start, utf16_offset(text, word.end) - start);
                let checker = shared();
                // SAFETY: on the main thread; the word's language as the checker identifies
                // it (its own language when it cannot), then its guesses: an array of strings.
                unsafe {
                    let language: Option<Retained<NSString>> = msg_send![
                        &*checker,
                        languageForWordRange: range,
                        inString: &*string,
                        orthography: std::ptr::null::<AnyObject>()
                    ];
                    let language = match language {
                        Some(l) => l,
                        None => msg_send![&*checker, language],
                    };
                    let guesses: Option<Retained<AnyObject>> = msg_send![
                        &*checker,
                        guessesForWordRange: range,
                        inString: &*string,
                        language: &*language,
                        inSpellDocumentWithTag: self.tag
                    ];
                    let Some(guesses) = guesses else { return Vec::new() };
                    let count: usize = msg_send![&*guesses, count];
                    (0..count)
                        .map(|i| {
                            let guess: *mut NSString = msg_send![&*guesses, objectAtIndex: i];
                            (*guess).to_string()
                        })
                        .collect()
                }
            })
        }

        fn learn(&mut self, word: &str) {
            // SAFETY: on the main thread; the word goes in the user's dictionary.
            unsafe {
                let _: () = msg_send![&*shared(), learnWord: &*NSString::from_str(word)];
            }
        }

        fn ignore(&mut self, word: &str) {
            // SAFETY: on the main thread; ignored in this checker's document only.
            unsafe {
                let _: () = msg_send![
                    &*shared(),
                    ignoreWord: &*NSString::from_str(word),
                    inSpellDocumentWithTag: self.tag
                ];
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod win {
    use std::ops::Range;

    use keleuo_core::spelling::utf8_offset;
    use windows::Win32::Globalization::{
        CORRECTIVE_ACTION_GET_SUGGESTIONS, CORRECTIVE_ACTION_REPLACE, GetUserDefaultLocaleName, ISpellChecker,
        ISpellCheckerFactory, SpellCheckerFactory,
    };
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree};
    use windows::core::{HSTRING, PWSTR};

    /// The language when the user's has no checker.
    const FALLBACK: &str = "en-US";

    /// The Spell Checking API (Windows 8 and later), in the user's language. Each message
    /// box has a checker of its own, so Ignore Spelling stays in it; Learn Spelling goes in
    /// the user's dictionary. Without the API, nothing is flagged.
    pub struct Checker(Option<ISpellChecker>);

    impl Checker {
        pub fn new() -> Self {
            // SAFETY: on the UI thread, where COM is (or now is) single-threaded; a changed
            // mode is fine, as COM is then already set up. The factory and checker are COM
            // objects released when dropped.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                let factory: Option<ISpellCheckerFactory> = CoCreateInstance(&SpellCheckerFactory, None, CLSCTX_INPROC_SERVER).ok();
                Self(factory.and_then(|factory| {
                    let mut name = [0u16; 85];
                    let len = GetUserDefaultLocaleName(&mut name);
                    let user = (len > 1).then(|| String::from_utf16_lossy(&name[..len as usize - 1]));
                    let supported = |tag: &str| factory.IsSupported(&HSTRING::from(tag)).is_ok_and(|b| b.as_bool());
                    let tag = user.filter(|t| supported(t.as_str())).unwrap_or_else(|| FALLBACK.to_string());
                    factory.CreateSpellChecker(&HSTRING::from(tag)).ok()
                }))
            }
        }
    }

    impl super::Source for Checker {
        fn misspelled(&mut self, text: &str) -> Option<Vec<Range<usize>>> {
            let Some(checker) = &self.0 else { return Some(Vec::new()) };
            let mut found = Vec::new();
            // SAFETY: the errors are enumerated until there are no more; each is a COM
            // object released when dropped. Repeated words (Delete) are not spelling.
            unsafe {
                let Ok(errors) = checker.ComprehensiveCheck(&HSTRING::from(text)) else { return Some(found) };
                loop {
                    let mut error = None;
                    if errors.Next(&mut error).is_err() {
                        break;
                    }
                    let Some(error) = error else { break };
                    let action = error.CorrectiveAction();
                    if !matches!(action, Ok(a) if a == CORRECTIVE_ACTION_GET_SUGGESTIONS || a == CORRECTIVE_ACTION_REPLACE) {
                        continue;
                    }
                    if let (Ok(start), Ok(len)) = (error.StartIndex(), error.Length()) {
                        let (start, len) = (start as usize, len as usize);
                        found.push(utf8_offset(text, start)..utf8_offset(text, start + len));
                    }
                }
            }
            Some(found)
        }

        fn guesses(&mut self, text: &str, word: Range<usize>) -> Vec<String> {
            let Some(checker) = &self.0 else { return Vec::new() };
            let mut out = Vec::new();
            // SAFETY: the suggestions are enumerated one at a time; each string is the
            // caller's to free with CoTaskMemFree.
            unsafe {
                let Ok(guesses) = checker.Suggest(&HSTRING::from(&text[word])) else { return out };
                loop {
                    let mut one = [PWSTR::null()];
                    let mut got = 0u32;
                    if guesses.Next(&mut one, Some(&raw mut got)).is_err() || got == 0 {
                        break;
                    }
                    out.push(one[0].to_string().unwrap_or_default());
                    CoTaskMemFree(Some(one[0].0 as *const _));
                }
            }
            out
        }

        fn learn(&mut self, word: &str) {
            if let Some(checker) = &self.0 {
                // SAFETY: a plain call on a live checker.
                let _ = unsafe { checker.Add(&HSTRING::from(word)) };
            }
        }

        fn ignore(&mut self, word: &str) {
            if let Some(checker) = &self.0 {
                // SAFETY: a plain call on a live checker.
                let _ = unsafe { checker.Ignore(&HSTRING::from(word)) };
            }
        }
    }
}
