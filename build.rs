fn main() {
    println!("cargo:rerun-if-changed=assets/mangadex-dl.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/mangadex-dl.ico");
        res.compile().expect("не удалось вшить иконку в .exe");
    }
}
