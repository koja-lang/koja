fn main() {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();

    let src = match (arch.as_str(), os.as_str()) {
        ("aarch64", _) => "src/arch/aarch64.s",
        ("x86_64", "windows") => "src/arch/x86_64_win.s",
        ("x86_64", _) => "src/arch/x86_64_sysv.s",
        _ => panic!("unsupported target: {arch}-{os}"),
    };

    let mut build = cc::Build::new();
    build.file(src);
    if os == "macos" {
        build.flag("-mmacosx-version-min=11.0");
    }
    build.compile("expo_context");
}
