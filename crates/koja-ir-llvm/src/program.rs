//! Compile a sealed [`IRProgram`] into the borrowed [`EmitContext`]'s
//! module through the shared [`compile_packages`] sequence. The host
//! `main` is the trampoline that spawns the `ProcessEntryWrapper` and
//! returns the stored exit code
//! ([`crate::main_wrapper::emit_process_entry_main`]).

use koja_ir::IRProgram;

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::pipeline::{EntryShape, compile_packages};

pub(crate) fn compile_program(
    ctx: &EmitContext<'_>,
    program: &IRProgram,
    app_name: &str,
) -> Result<(), LlvmError> {
    compile_packages(
        ctx,
        &program.packages,
        &program.built_constant_order,
        app_name,
        EntryShape::Process(program.entry_function()),
    )
}
