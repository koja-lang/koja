//! Native linker glue for the LLVM backend.
//!
//! [`expo-ir-llvm`](../expo_ir_llvm/index.html) emits a
//! `.o` file; this module hands that object to `cc` along with the
//! embedded runtime archive (and bundled BoringSSL `libcrypto.a` /
//! `libssl.a` so `@link "ssl"` resolves without the user wiring up
//! `LIBRARY_PATH`) and produces the final binary at `output`.
//!
//! All callers go through [`link`], which is the sole public entry
//! point. Knobs that change linker behavior (release/debug,
//! verbosity) live on [`LinkOptions`].

use std::path::Path;
#[cfg(target_os = "macos")]
use std::sync::OnceLock;
use std::{env, fs, process};

/// Knobs for [`link`]: release strips macOS dSYMs; `quiet`
/// suppresses the trailing `compiled: <output>` line that
/// `expo build` prints (used by `expo run` so its output stays
/// the user binary's stdout).
#[derive(Clone, Copy)]
pub(crate) struct LinkOptions {
    pub release: bool,
    pub quiet: bool,
}

/// Embedded static libraries written to the temp link directory.
/// The runtime is always linked; BoringSSL ships alongside so
/// `@link "ssl"` / `@link "crypto"` annotations resolve out of
/// the box.
const EMBEDDED_RUNTIME: &[u8] = include_bytes!(env!("EXPO_RUNTIME_LIB_PATH"));
const EMBEDDED_CRYPTO: &[u8] = include_bytes!(env!("EXPO_CRYPTO_LIB_PATH"));
const EMBEDDED_SSL: &[u8] = include_bytes!(env!("EXPO_SSL_LIB_PATH"));

/// Returns the macOS product version (e.g. "26.4") for use as
/// `MACOSX_DEPLOYMENT_TARGET`. Cached so `sw_vers` is invoked at
/// most once per process.
#[cfg(target_os = "macos")]
fn macos_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| {
                let s = s.trim();
                let parts: Vec<&str> = s.splitn(3, '.').collect();
                if parts.len() >= 2 {
                    format!("{}.{}", parts[0], parts[1])
                } else {
                    s.to_string()
                }
            })
            .unwrap_or_else(|| "11.0".to_string())
    })
}

/// Links an object file with the embedded runtime library to
/// produce an executable. `link_libraries` carries `@link "name"`
/// annotations collected during lowering (passed as `-l<name>`);
/// `extra_lib_search_paths` lets callers add directories the
/// linker should scan for `-l<name>` resolution (passed as
/// `-L<dir>`). Project-mode callers thread the directory holding
/// `expo.toml` through so a sibling `libfoo.a` is discoverable
/// without the user manually setting `LIBRARY_PATH` or running
/// from a specific `cwd`. The embedded-archive temp dir is always
/// added on top of these so the runtime / crypto archives stay
/// resolvable.
pub(crate) fn link(
    obj_path: &str,
    output: &str,
    link_libraries: &[String],
    extra_lib_search_paths: &[&Path],
    options: LinkOptions,
) {
    #[cfg(not(target_os = "macos"))]
    let _ = options.release;

    let tmp_dir = env::temp_dir().join(format!("expo-link-{}", process::id()));
    fs::create_dir_all(&tmp_dir).expect("failed to create temp dir for linking");

    fs::write(tmp_dir.join("libexpo_runtime.a"), EMBEDDED_RUNTIME)
        .expect("failed to write embedded runtime library");
    fs::write(tmp_dir.join("libcrypto.a"), EMBEDDED_CRYPTO)
        .expect("failed to write embedded crypto library");
    fs::write(tmp_dir.join("libssl.a"), EMBEDDED_SSL)
        .expect("failed to write embedded ssl library");

    let tmp_dir_str = tmp_dir.to_string_lossy();

    let mut args = vec![
        obj_path.to_string(),
        "-lexpo_runtime".to_string(),
        "-L".to_string(),
        tmp_dir_str.to_string(),
        "-o".to_string(),
        output.to_string(),
    ];
    for path in extra_lib_search_paths {
        args.push("-L".to_string());
        args.push(path.to_string_lossy().to_string());
    }
    // Modern Debian/Ubuntu default `cc` to PIE, which rejects the
    // absolute (`R_X86_64_32`) relocations LLVM emits under
    // `RelocMode::Default`. Until codegen is switched to
    // `RelocMode::PIC`, ask the linker for a non-PIE binary on
    // Linux.
    #[cfg(target_os = "linux")]
    args.push("-no-pie".to_string());
    for lib in link_libraries {
        args.push(format!("-l{lib}"));
    }

    let mut cmd = process::Command::new("cc");
    cmd.args(&args);
    cmd.stderr(process::Stdio::piped());
    #[cfg(target_os = "macos")]
    {
        cmd.env("MACOSX_DEPLOYMENT_TARGET", macos_version());
    }

    let cleanup = |tmp: &Path, obj: &str| {
        let _ = fs::remove_dir_all(tmp);
        let _ = fs::remove_file(obj);
    };

    let link_output = cmd.output().unwrap_or_else(|e| {
        eprintln!("failed to run linker: {e}");
        cleanup(&tmp_dir, obj_path);
        process::exit(1);
    });

    let stderr = String::from_utf8_lossy(&link_output.stderr);
    for line in stderr.lines() {
        if !line.contains("reexported library") {
            eprintln!("{line}");
        }
    }

    if !link_output.status.success() {
        eprintln!(
            "linker failed with exit code: {}",
            link_output.status.code().unwrap_or(-1)
        );
        cleanup(&tmp_dir, obj_path);
        process::exit(1);
    }

    #[cfg(target_os = "macos")]
    if !options.release {
        let _ = process::Command::new("dsymutil")
            .arg(output)
            .stderr(process::Stdio::null())
            .status();
    }
    cleanup(&tmp_dir, obj_path);
    if !options.quiet {
        println!("compiled: {output}");
    }
}
