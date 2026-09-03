//! Native object-file emission via inkwell's `TargetMachine`.

use std::path::Path;

use inkwell::OptimizationLevel;
use inkwell::module::{FlagBehavior, Module};
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::FileType;

use crate::ctx::EmitContext;
use crate::error::LlvmError;

/// Run the optimization pipeline at `opt_level` on the context's
/// module and write it to `path` as a native object file through the
/// context's target machine. At `OptimizationLevel::None` no
/// middle-end passes run (debug builds). Release builds pass
/// `Aggressive` to engage the full pipeline.
pub(crate) fn emit_object_file(
    ctx: &EmitContext<'_>,
    path: &Path,
    opt_level: OptimizationLevel,
) -> Result<(), LlvmError> {
    mark_position_independent_executable(&ctx.module);

    if let Some(passes) = passes_for(opt_level) {
        ctx.module
            .run_passes(passes, &ctx.target_machine, PassBuilderOptions::create())
            .map_err(|e| LlvmError::ObjectEmit(format!("optimization passes failed: {e}")))?;
    }

    ctx.target_machine
        .write_to_file(&ctx.module, FileType::Object, path)
        .map_err(|e| LlvmError::ObjectEmit(format!("failed to write object file: {e}")))
}

/// Stamp the module flags clang sets under `-fpie`, recording that
/// this module targets an executable, not a shared library. The C
/// API cannot mark globals `dso_local`, so under `RelocMode::PIC`
/// even our own definitions get GOT/PLT-style references. The linker
/// relaxes those to direct access, since symbols defined in an
/// executable cannot be preempted.
fn mark_position_independent_executable(module: &Module<'_>) {
    let two = module.get_context().i32_type().const_int(2, false);
    module.add_basic_value_flag("PIC Level", FlagBehavior::Error, two);
    module.add_basic_value_flag("PIE Level", FlagBehavior::Error, two);
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
