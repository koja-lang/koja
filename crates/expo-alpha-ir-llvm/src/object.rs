//! Native object-file emission via inkwell's `TargetMachine`.
//! Mirrors the v1 codegen pattern in `expo-codegen`'s
//! `emit_object_file`, trimmed to non-release defaults.

use std::path::Path;

use inkwell::OptimizationLevel;
use inkwell::module::Module;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};

use crate::error::LlvmError;

/// Initialize the native target, build a target machine for the host
/// triple + CPU + features, and write `module` to `path` as a native
/// object file. Always emits at `OptimizationLevel::None` for the
/// alpha slice — release-mode optimization is a follow-up.
pub(crate) fn emit_object_file(module: &Module<'_>, path: &Path) -> Result<(), LlvmError> {
    Target::initialize_native(&InitializationConfig::default())
        .map_err(|e| LlvmError::ObjectEmit(format!("failed to initialize native target: {e}")))?;

    let triple = TargetMachine::get_default_triple();
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
            OptimizationLevel::None,
            RelocMode::Default,
            CodeModel::Default,
        )
        .ok_or_else(|| LlvmError::ObjectEmit("failed to create target machine".to_string()))?;

    machine
        .write_to_file(module, FileType::Object, path)
        .map_err(|e| LlvmError::ObjectEmit(format!("failed to write object file: {e}")))
}
