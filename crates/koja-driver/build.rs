use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let lib_path = PathBuf::from(&out_dir)
        .ancestors()
        .map(|p| p.join("libkoja_runtime.a"))
        .find(|p| p.exists())
        .unwrap_or_else(|| {
            panic!(
                "libkoja_runtime.a not found searching parents of {}. Build koja-runtime first.",
                out_dir
            )
        });

    println!(
        "cargo:rustc-env=KOJA_RUNTIME_LIB_PATH={}",
        lib_path.display()
    );
    println!("cargo:rerun-if-changed={}", lib_path.display());

    let build_dir = PathBuf::from(&out_dir)
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == "build"))
        .map(|p| p.to_path_buf())
        .expect("could not find Cargo build directory");

    let crypto_lib_path = find_file(&build_dir, "libcrypto.a").unwrap_or_else(|| {
        panic!(
            "libcrypto.a not found under {}. boring-sys should have built it.",
            build_dir.display()
        )
    });
    println!(
        "cargo:rustc-env=KOJA_CRYPTO_LIB_PATH={}",
        crypto_lib_path.display()
    );
    println!("cargo:rerun-if-changed={}", crypto_lib_path.display());

    let ssl_lib_path = find_file(&build_dir, "libssl.a").unwrap_or_else(|| {
        panic!(
            "libssl.a not found under {}. boring-sys should have built it.",
            build_dir.display()
        )
    });
    println!(
        "cargo:rustc-env=KOJA_SSL_LIB_PATH={}",
        ssl_lib_path.display()
    );
    println!("cargo:rerun-if-changed={}", ssl_lib_path.display());

    println!("cargo:rerun-if-changed=build.rs");
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if path.is_file() && path.file_name().is_some_and(|n| n == name) {
            return Some(path);
        }
        if path.is_dir()
            && let Some(found) = find_file(&path, name)
        {
            return Some(found);
        }
    }
    None
}
