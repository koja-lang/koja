//! Compile a sealed [`IRScript`] into the borrowed [`EmitCtx`]'s
//! module: emit the runtime-name global, declare every helper,
//! synthesize the script body as `main` (with the auto-print
//! wrapper), then define each helper's body.
//!
//! Same shape as [`crate::program::compile_program`] minus the
//! "skip the entry function" step — script-mode has no `fn main`
//! item; `script.blocks` is the body that becomes `main`.

use expo_alpha_ir::IRScript;

use crate::ctx::EmitCtx;
use crate::error::LlvmError;
use crate::function::{declare_function, define_function};
use crate::main_wrapper::{emit_app_name_global, emit_as_main};

pub(crate) fn compile_script(
    ctx: &EmitCtx<'_>,
    script: &IRScript,
    app_name: &str,
) -> Result<(), LlvmError> {
    emit_app_name_global(ctx, app_name);
    let mut declared = Vec::with_capacity(script.packages.iter().map(|p| p.functions.len()).sum());
    for package in &script.packages {
        for function in package.functions.values() {
            declared.push((function, declare_function(ctx, function)?));
        }
    }
    emit_as_main(ctx, &script.blocks, &script.return_type)?;
    for (function, llvm_function) in declared {
        define_function(ctx, function, llvm_function)?;
    }
    Ok(())
}
