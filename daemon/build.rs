fn main() {
    // Emit a cfg flag if the tray icon PNG is present so tray.rs can
    // conditionally include it. The build succeeds without the file — the
    // tray just shows no icon.
    if std::path::Path::new("assets/tray.png").exists() {
        println!("cargo:rustc-cfg=has_tray_icon");
    }
    // Re-run if the asset changes or appears
    println!("cargo:rerun-if-changed=assets/tray.png");
}
