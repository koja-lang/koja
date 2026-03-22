use std::path::PathBuf;

fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let lib_path = PathBuf::from(&out_dir)
        .ancestors()
        .map(|p| p.join("libexpo_runtime.a"))
        .find(|p| p.exists())
        .unwrap_or_else(|| {
            panic!(
                "libexpo_runtime.a not found searching parents of {}. Build expo-runtime first.",
                out_dir
            )
        });

    println!(
        "cargo:rustc-env=EXPO_RUNTIME_LIB_PATH={}",
        lib_path.display()
    );

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", lib_path.display());
}
