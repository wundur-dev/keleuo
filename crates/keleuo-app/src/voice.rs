//! Speaking a message: the 🎤 in the message box listens, and what is said is typed into the
//! box as it is heard, with Apple's speech recognition (as macOS dictation: on the Mac or
//! Apple's servers, as Apple decides) on the microphone's audio. macOS asks once for the microphone and for speech recognition.
//! Results come on the system's threads; they reach the chat through a channel ([`Heard`]).
//! Not on Windows or Linux yet (a local Whisper may come; see the open queue).

#[cfg(not(target_os = "macos"))]
use std::sync::mpsc::Sender;

/// What the recognition says, for the chat to act on.
#[derive(Debug)]
pub enum Heard {
    /// Speech recognition is allowed (or not).
    Allowed(bool),
    /// What was heard so far (`done`: all of it).
    Text { text: String, done: bool },
    /// It stopped; why.
    Failed(String),
}

/// Whether this system can listen.
pub fn available() -> bool {
    cfg!(target_os = "macos")
}

#[cfg(target_os = "macos")]
pub use mac::{Listening, ask, listen};

#[cfg(target_os = "macos")]
mod mac {
    use std::ptr::NonNull;
    use std::sync::mpsc::Sender;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2_avf_audio::{AVAudioEngine, AVAudioPCMBuffer, AVAudioTime};
    use objc2_foundation::NSError;
    use objc2_speech::{
        SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult, SFSpeechRecognitionTask, SFSpeechRecognizer,
        SFSpeechRecognizerAuthorizationStatus,
    };

    use super::Heard;

    type Tap = RcBlock<dyn Fn(NonNull<AVAudioPCMBuffer>, NonNull<AVAudioTime>)>;
    type Handler = RcBlock<dyn Fn(*mut SFSpeechRecognitionResult, *mut NSError)>;

    /// Ask for speech recognition (macOS shows its question the first time); the answer
    /// comes as [`Heard::Allowed`].
    pub fn ask(tx: Sender<Heard>) {
        let block = RcBlock::new(move |status: SFSpeechRecognizerAuthorizationStatus| {
            let _ = tx.send(Heard::Allowed(status == SFSpeechRecognizerAuthorizationStatus::Authorized));
        });
        // SAFETY: the block is copied by the system and called once.
        unsafe { SFSpeechRecognizer::requestAuthorization(&block) };
    }

    /// The microphone, listened to: its audio goes to the recognition until [`Listening::finish`].
    pub struct Listening {
        engine: Retained<AVAudioEngine>,
        request: Retained<SFSpeechAudioBufferRecognitionRequest>,
        task: Retained<SFSpeechRecognitionTask>,
        _recognizer: Retained<SFSpeechRecognizer>,
        _tap: Tap,
        _handler: Handler,
        finished: std::cell::Cell<bool>,
    }

    /// Start listening (speech recognition allowed); what is heard comes as [`Heard::Text`].
    pub fn listen(tx: Sender<Heard>) -> Result<Listening, String> {
        // SAFETY: plain Objective-C calls on objects made here, on the main thread.
        unsafe {
            let recognizer = SFSpeechRecognizer::new();
            if !recognizer.isAvailable() {
                return Err("Speech recognition isn't available right now".into());
            }
            let request = SFSpeechAudioBufferRecognitionRequest::new();
            request.setShouldReportPartialResults(true);
            request.setAddsPunctuation(true);
            let engine = AVAudioEngine::new();
            let input = engine.inputNode();
            let format = input.outputFormatForBus(0);
            let feed = request.clone();
            let tap: Tap = RcBlock::new(move |buffer: NonNull<AVAudioPCMBuffer>, _: NonNull<AVAudioTime>| {
                feed.appendAudioPCMBuffer(buffer.as_ref());
            });
            input.installTapOnBus_bufferSize_format_block(0, 1024, Some(&format), RcBlock::as_ptr(&tap));
            engine.prepare();
            if let Err(e) = engine.startAndReturnError() {
                input.removeTapOnBus(0);
                return Err(format!("The microphone could not be opened: {}", e.localizedDescription()));
            }
            let handler: Handler = RcBlock::new(move |result: *mut SFSpeechRecognitionResult, error: *mut NSError| {
                if let Some(result) = result.as_ref() {
                    let text = result.bestTranscription().formattedString().to_string();
                    let _ = tx.send(Heard::Text { text, done: result.isFinal() });
                } else if let Some(error) = error.as_ref() {
                    let _ = tx.send(Heard::Failed(error.localizedDescription().to_string()));
                }
            });
            let task = recognizer.recognitionTaskWithRequest_resultHandler(&request, &handler);
            Ok(Listening { engine, request, task, _recognizer: recognizer, _tap: tap, _handler: handler, finished: std::cell::Cell::new(false) })
        }
    }

    impl Listening {
        /// Stop listening: what was said so far is recognised to the end (its last result is
        /// `done`).
        pub fn finish(&self) {
            if self.finished.replace(true) {
                return;
            }
            // SAFETY: the engine, its input and the request are this listening's.
            unsafe {
                self.engine.stop();
                self.engine.inputNode().removeTapOnBus(0);
                self.request.endAudio();
            }
        }
    }

    impl Drop for Listening {
        fn drop(&mut self) {
            self.finish();
            // SAFETY: the task is this listening's.
            unsafe { self.task.cancel() };
        }
    }
}

/// Elsewhere: nothing to listen with yet.
#[cfg(not(target_os = "macos"))]
pub struct Listening;

#[cfg(not(target_os = "macos"))]
impl Listening {
    pub fn finish(&self) {}
}

#[cfg(not(target_os = "macos"))]
pub fn ask(tx: Sender<Heard>) {
    let _ = tx.send(Heard::Allowed(false));
}

#[cfg(not(target_os = "macos"))]
pub fn listen(_tx: Sender<Heard>) -> Result<Listening, String> {
    Err("Speaking a message isn't available on this system yet".into())
}
