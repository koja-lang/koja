//! Native object-file emission via inkwell's `TargetMachine`.
//! Mirrors the v1 codegen pattern in `koja-codegen`'s
//! `emit_object_file`.

use std::path::Path;

use inkwell::OptimizationLevel;
use inkwell::module::Module;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple,
};

use crate::error::LlvmError;

/// Default macOS deployment target baked into the emitted object's
/// triple when `MACOSX_DEPLOYMENT_TARGET` is unset. Matches
/// `koja-driver/src/link.rs::DEFAULT_MACOS_DEPLOYMENT_TARGET` and
/// the workspace `MACOSX_DEPLOYMENT_TARGET` so user binaries link
/// without `ld: warning: object file ... built for newer macOS
/// version` mismatches when the host SDK is newer than the floor.
#[cfg(target_os = "macos")]
const DEFAULT_MACOS_DEPLOYMENT_TARGET: &str = "11.0";

/// LLVM triple arch component for the macOS host architecture.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const MACOS_TARGET_ARCH: &str = "arm64";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const MACOS_TARGET_ARCH: &str = "x86_64";

/// Initialize the native target, build a target machine for the host
/// triple + CPU + features, run the optimization pipeline at
/// `opt_level`, and write `module` to `path` as a native object file.
/// At `OptimizationLevel::None` no middle-end passes run (debug
/// builds); release builds pass `Aggressive` to engage the full
/// pipeline.
pub(crate) fn emit_object_file(
    module: &Module<'_>,
    path: &Path,
    opt_level: OptimizationLevel,
) -> Result<(), LlvmError> {
    Target::initialize_native(&InitializationConfig::default())
        .map_err(|e| LlvmError::ObjectEmit(format!("failed to initialize native target: {e}")))?;

    let triple = host_triple();
    let target = Target::from_triple(&triple)
        .map_err(|e| LlvmError::ObjectEmit(format!("failed to get target: {e}")))?;

    // LLVM 18's X86 backend requires a real CPU name (not "generic")
    // because it constructs a fresh `X86Subtarget` per function during
    // emission and indexes into scheduling tables that are only
    // populated for known CPU models. v1 codegen learned this the
    // hard way; mirroring its host-CPU selection avoids the same
    // SIGSEGV on Linux x86_64.
    let cpu = TargetMachine::get_host_cpu_name().to_string();
    let features = TargetMachine::get_host_cpu_features().to_string();
    let machine = target
        .create_target_machine(
            &triple,
            &cpu,
            &features,
            opt_level,
            RelocMode::Default,
            CodeModel::Default,
        )
        .ok_or_else(|| LlvmError::ObjectEmit("failed to create target machine".to_string()))?;

    if let Some(passes) = passes_for(opt_level) {
        module
            .run_passes(passes, &machine, PassBuilderOptions::create())
            .map_err(|e| LlvmError::ObjectEmit(format!("optimization passes failed: {e}")))?;
    }

    machine
        .write_to_file(module, FileType::Object, path)
        .map_err(|e| LlvmError::ObjectEmit(format!("failed to write object file: {e}")))
}

/// Map an [`OptimizationLevel`] to a new-PM pass-pipeline string for
/// [`Module::run_passes`]. `None` skips the pipeline entirely so debug
/// builds stay at `-O0`.
fn passes_for(level: OptimizationLevel) -> Option<&'static str> {
    match level {
        OptimizationLevel::None => None,
        OptimizationLevel::Less => Some("default<O1>"),
        OptimizationLevel::Default => Some("default<O2>"),
        OptimizationLevel::Aggressive => Some("default<O3>"),
    }
}

/// Returns the LLVM triple the emitted object file declares. On
/// macOS, pin the deployment-target portion (honoring
/// `MACOSX_DEPLOYMENT_TARGET` if the caller has set one, otherwise
/// [`DEFAULT_MACOS_DEPLOYMENT_TARGET`]) so the bundled crypto
/// archives and the user binary land on the same floor; elsewhere
/// fall back to whatever LLVM thinks the host is.
fn host_triple() -> TargetTriple {
    #[cfg(target_os = "macos")]
    {
        let version = std::env::var("MACOSX_DEPLOYMENT_TARGET")
            .unwrap_or_else(|_| DEFAULT_MACOS_DEPLOYMENT_TARGET.to_string());
        TargetTriple::create(&format!("{MACOS_TARGET_ARCH}-apple-macosx{version}"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        TargetMachine::get_default_triple()
    }
}
