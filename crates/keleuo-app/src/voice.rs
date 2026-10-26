//! Speaking a message: the 🎤 in the message box starts the system's own dictation into it
//! (what it hears is typed where the text cursor is, as the system types it anywhere). macOS:
//! Start Dictation, the Edit menu's command, sent to the focused view. Windows: voice typing
//! (Win+H). Linux has none built in: no button there (a local Whisper may come later).

/// Whether this system has dictation to start.
pub fn available() -> bool {
    cfg!(any(target_os = "macos", windows))
}

/// Start the system's dictation into the focused text field. Why not, when it could not.
#[cfg(target_os = "macos")]
pub fn start() -> Result<(), String> {
    use objc2::MainThreadMarker;
    use objc2::sel;
    use objc2_app_kit::NSApplication;
    let mtm = MainThreadMarker::new().ok_or("not on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: on the main thread; the action takes the sender, here none.
    let sent = unsafe { app.sendAction_to_from(sel!(startDictation:), None, None) };
    if sent { Ok(()) } else { Err("Dictation did not start: turn it on in System Settings > Keyboard > Dictation, or press the 🎤 key".into()) }
}

/// Start the system's dictation into the focused text field: Win+H, voice typing.
#[cfg(windows)]
pub fn start() -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_H, VK_LWIN,
    };
    let key = |vk: VIRTUAL_KEY, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, time: 0, dwExtraInfo: 0 } },
    };
    let keys = [key(VK_LWIN, false), key(VK_H, false), key(VK_H, true), key(VK_LWIN, true)];
    // SAFETY: plain keyboard input, the size of the structure given.
    let sent = unsafe { SendInput(&keys, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize == keys.len() { Ok(()) } else { Err("Voice typing did not start: press Win+H".into()) }
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn start() -> Result<(), String> {
    Err("This system has no dictation built in".into())
}
