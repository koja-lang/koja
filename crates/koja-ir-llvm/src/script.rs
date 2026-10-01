//! Compile a sealed [`IRScript`] into the borrowed [`EmitContext`]'s
//! module through the shared [`compile_packages`] sequence. The host
//! `main` runs `script.blocks` as PID 1
//! ([`crate::main_wrapper::emit_script_main`]), since script mode has
//! no `fn main` item.

use koja_ir::IRScript;

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::pipeline::{EntryShape, compile_packages};

pub(crate) fn compile_script(
    ctx: &EmitContext<'_>,
    script: &IRScript,
    app_name: &str,
) -> Result<(), LlvmError> {
    compile_packages(
        ctx,
        &script.packages,
        &script.built_constant_order,
        app_name,
        EntryShape::Script {
            blocks: &script.blocks,
            def_location: script.def_location.as_ref(),
        },
    )
}
