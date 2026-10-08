//! Embeds the exe icon, the manifest (DPI awareness, UTF-8) and version info on Windows.

fn main() {
    println!("cargo:rerun-if-changed=assets/icons/app.ico");
    println!("cargo:rerun-if-changed=assets/app.manifest");
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icons/app.ico");
    res.set_manifest_file("assets/app.manifest");
    res.set("ProductName", "VoicePaste");
    res.set("FileDescription", "VoicePaste: local voice-to-text");
    res.set("InternalName", "voicepaste");
    res.set("OriginalFilename", "voicepaste.exe");
    res.set("LegalCopyright", "VoicePaste");
    if let Err(e) = res.compile() {
        // A missing resource compiler should not make the whole build fail.
        println!("cargo:warning=could not embed icon and manifest: {e}");
    }
}
