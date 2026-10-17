//! Gives the Windows .exe its icon (Explorer, desktop shortcuts), from
//! assets/icon.ico. The running window's icon on every platform is set
//! separately, from assets/icon.png, in `gui::run`.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    // The target, not the host: cross-builds for Windows get the icon,
    // and Linux/macOS builds skip this.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.compile().expect("failed to embed assets/icon.ico");
    }
}
