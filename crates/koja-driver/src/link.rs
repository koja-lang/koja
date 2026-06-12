//! Native linker glue for the LLVM backend.
//!
//! [`koja-ir-llvm`](../koja_ir_llvm/index.html) emits a
//! `.o` file; this module hands that object to `cc` along with the
//! embedded runtime archive (and bundled BoringSSL `libcrypto.a` /
//! `libssl.a` so `@link "ssl"` resolves without the user wiring up
//! `LIBRARY_PATH`) and produces the final binary at `output`.
//!
//! All callers go through [`link`], which is the sole public entry
//! point. Knobs that change linker behavior (release/debug,
//! verbosity) live on [`LinkOptions`].

use std::path::Path;
use std::{env, fs, process};

/// Knobs for [`link`]: release strips macOS dSYMs; `quiet`
/// suppresses the trailing `compiled: <output>` line that
/// `koja build` prints (used by `koja run` so its output stays
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
const EMBEDDED_RUNTIME: &[u8] = include_bytes!(env!("KOJA_RUNTIME_LIB_PATH"));
const EMBEDDED_CRYPTO: &[u8] = include_bytes!(env!("KOJA_CRYPTO_LIB_PATH"));
const EMBEDDED_SSL: &[u8] = include_bytes!(env!("KOJA_SSL_LIB_PATH"));

/// Default deployment target passed to the linker via
/// `MACOSX_DEPLOYMENT_TARGET` when the env var is unset. Matches the
/// floor used when compiling the embedded runtime
/// (`-mmacosx-version-min=11.0` in `koja-runtime/build.rs`) and the
/// workspace-wide `MACOSX_DEPLOYMENT_TARGET` in
/// `koja/.cargo/config.toml` that drives `boring-sys`'s
/// `libcrypto.a` / `libssl.a` builds. Using `sw_vers` here is
/// tempting but produces "object file was built for newer macOS
/// version than being linked" warnings whenever the installed Xcode
/// SDK is newer than the host system version.
#[cfg(target_os = "macos")]
const DEFAULT_MACOS_DEPLOYMENT_TARGET: &str = "11.0";

/// Resolves the macOS deployment target for the link step. Honors
/// a caller-supplied `MACOSX_DEPLOYMENT_TARGET` env var (matching
/// `cc` / `clang` / `rustc` convention) and falls back to
/// [`DEFAULT_MACOS_DEPLOYMENT_TARGET`].
#[cfg(target_os = "macos")]
fn macos_deployment_target() -> String {
    env::var("MACOSX_DEPLOYMENT_TARGET")
        .unwrap_or_else(|_| DEFAULT_MACOS_DEPLOYMENT_TARGET.to_string())
}

/// Links an object file with the embedded runtime library to
/// produce an executable. `link_libraries` carries `@link "name"`
/// annotations collected during lowering (passed as `-l<name>`);
/// `extra_lib_search_paths` lets callers add directories the
/// linker should scan for `-l<name>` resolution (passed as
/// `-L<dir>`). Project-mode callers thread the directory holding
/// `koja.toml` through so a sibling `libfoo.a` is discoverable
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

    let tmp_dir = env::temp_dir().join(format!("koja-link-{}", process::id()));
    fs::create_dir_all(&tmp_dir).expect("failed to create temp dir for linking");

    fs::write(tmp_dir.join("libkoja_runtime.a"), EMBEDDED_RUNTIME)
        .expect("failed to write embedded runtime library");
    fs::write(tmp_dir.join("libcrypto.a"), EMBEDDED_CRYPTO)
        .expect("failed to write embedded crypto library");
    fs::write(tmp_dir.join("libssl.a"), EMBEDDED_SSL)
        .expect("failed to write embedded ssl library");

    let tmp_dir_str = tmp_dir.to_string_lossy();

    let mut args = vec![
        obj_path.to_string(),
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
    // GNU ld resolves static archives in a single left-to-right
    // pass, so inter-archive references break when the archives
    // appear in the wrong order (`libssl.a` pulls EVP_HPKE_* /
    // KYBER_* / spake2plus symbols from `libcrypto.a`). Group the
    // archives so ld re-scans them until no new references resolve;
    // macOS ld64 is order-independent and needs no grouping.
    #[cfg(target_os = "linux")]
    args.push("-Wl,--start-group".to_string());
    args.push("-lkoja_runtime".to_string());
    for lib in link_libraries {
        args.push(format!("-l{lib}"));
    }
    #[cfg(target_os = "linux")]
    args.push("-Wl,--end-group".to_string());
    // BoringSSL's libssl is C++ (libcrypto is plain C), so pull in
    // the C++ runtime whenever it is linked.
    if link_libraries.iter().any(|lib| lib == "ssl") {
        #[cfg(target_os = "macos")]
        args.push("-lc++".to_string());
        #[cfg(not(target_os = "macos"))]
        args.push("-lstdc++".to_string());
    }

    let mut cmd = process::Command::new("cc");
    cmd.args(&args);
    cmd.stderr(process::Stdio::piped());
    #[cfg(target_os = "macos")]
    {
        cmd.env("MACOSX_DEPLOYMENT_TARGET", macos_deployment_target());
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
