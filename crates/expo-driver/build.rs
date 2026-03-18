use std::path::PathBuf;

fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let target_dir = PathBuf::from(&out_dir)
        .ancestors()
        .find(|p| p.ends_with("target/debug") || p.ends_with("target/release"))
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from(&out_dir));

    let lib_path = target_dir.join("libexpo_runtime.a");
    if lib_path.exists() {
        println!(
            "cargo:rustc-env=EXPO_RUNTIME_LIB_PATH={}",
            lib_path.display()
        );
    } else {
        panic!(
            "libexpo_runtime.a not found at {}. Build expo-runtime first.",
            lib_path.display()
        );
    }

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", lib_path.display());
}
