//! Compile a sealed [`IRProgram`] into the borrowed [`EmitCtx`]'s
//! module: emit the runtime-name global, declare every non-entry
//! helper, synthesize the entry as `main` (with the auto-print
//! wrapper), then define each helper's body.
//!
//! The two-phase declare-then-define pattern lets mutually-recursive
//! calls resolve through `module.get_function` before either body has
//! been walked.

use expo_alpha_ir::IRProgram;

use crate::ctx::EmitCtx;
use crate::error::LlvmError;
use crate::function::{declare_function, define_function};
use crate::main_wrapper::{emit_app_name_global, emit_as_main};

pub(crate) fn compile_program(
    ctx: &EmitCtx<'_>,
    program: &IRProgram,
    app_name: &str,
) -> Result<(), LlvmError> {
    emit_app_name_global(ctx, app_name);
    let mut declared = Vec::with_capacity(program.packages.iter().map(|p| p.functions.len()).sum());
    for package in &program.packages {
        for function in package.functions.values() {
            if program.is_entry(function) {
                continue;
            }
            declared.push((function, declare_function(ctx, function)?));
        }
    }
    let entry = program.entry_function();
    emit_as_main(ctx, &entry.blocks, &entry.return_type)?;
    for (function, llvm_function) in declared {
        define_function(ctx, function, llvm_function)?;
    }
    Ok(())
}
