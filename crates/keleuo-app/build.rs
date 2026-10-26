//! On macOS the window's binary carries an Info.plist of its own (in a `__TEXT,__info_plist`
//! section), so that run without an app bundle (in development) it can ask for the microphone
//! and speech recognition: macOS stops a program that asks without saying why. The bundle's
//! Info.plist (scripts/bundle-macos.sh) says the same.

fn main() {
    let plist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/macos/Info.plist");
    println!("cargo:rerun-if-changed={}", plist.display());
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bin=keleuo=-Wl,-sectcreate,__TEXT,__info_plist,{}", plist.display());
    }
}
