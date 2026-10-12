//! The app's icon in the Dock and the app switcher while it runs without a bundle: the full
//! scene (the mark alone is for sizes of 64 px and under, in `assets/icon/Keleuo.icns`).

/// The icon at 512 px.
pub const PNG: &[u8] = include_bytes!("../assets/icon/keleuo-512.png");

#[cfg(target_os = "macos")]
pub fn set() {
    use objc2::AllocAnyThread;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;
    let Some(mtm) = MainThreadMarker::new() else { return };
    let data = NSData::with_bytes(PNG);
    let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else { return };
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: on the main thread, with a valid image.
    unsafe { app.setApplicationIconImage(Some(&image)) };
}

#[cfg(not(target_os = "macos"))]
pub fn set() {}
