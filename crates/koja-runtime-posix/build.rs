fn main() {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();

    let src = match arch.as_str() {
        "aarch64" => "src/arch/aarch64.s",
        "x86_64" => "src/arch/x86_64_sysv.s",
        _ => panic!("unsupported target: {arch}-{os}"),
    };

    // `cc` emits rerun-if-env-changed directives, which disables
    // cargo's rerun-on-any-package-change default. The native sources
    // must be declared explicitly or edits to them silently keep the
    // stale objects.
    println!("cargo:rerun-if-changed=src/arch");
    println!("cargo:rerun-if-changed=src/reductions.c");

    let mut build = cc::Build::new();
    build.file(src);
    build.file("src/reductions.c");
    if os == "macos" {
        build.flag("-mmacosx-version-min=11.0");
    }
    build.compile("koja_context");
}
