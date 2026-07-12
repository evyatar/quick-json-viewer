fn main() {
    // Embed the app icon into the .exe so Explorer and the taskbar show it.
    #[cfg(target_os = "windows")]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("src/icon.ico");
        res.compile().expect("failed to compile Windows resources");
    }
}
