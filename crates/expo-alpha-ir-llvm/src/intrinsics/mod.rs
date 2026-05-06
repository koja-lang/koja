//! Per-backend dispatch table for `@intrinsic` function bodies.
//! Mirrors the eval interpreter's [`expo_alpha_ir_eval::intrinsics`]
//! shape — each registered intrinsic is keyed by its full
//! [`expo_alpha_ir::IRSymbol::mangled`] name and routed to a
//! hand-written emitter that synthesizes the LLVM body.
//!
//! Adding a new intrinsic: drop a sibling `<name>.rs` module
//! exporting `pub(super) fn emit_<name>`, register it in
//! [`emitter_for`], and pin a 1-1 test in
//! `tests/intrinsics.rs`.

use expo_alpha_ir::IRFunction;
use inkwell::values::FunctionValue;

use crate::ctx::EmitContext;
use crate::error::LlvmError;

mod print;

use print::emit_global_print;

/// Function pointer type for an intrinsic's LLVM emitter. The
/// emitter receives the [`EmitContext`], the IR function (for params /
/// return type), and the already-declared `FunctionValue` whose body
/// it should fill in.
type IntrinsicEmitter<'ctx> =
    fn(&EmitContext<'ctx>, &IRFunction, FunctionValue<'ctx>) -> Result<(), LlvmError>;

/// Synthesize the body of an `@intrinsic` function. Looks up
/// `function.symbol.mangled()` in the dispatch table and forwards to
/// the registered emitter; unknown keys surface as an explicit
/// "unknown intrinsic" codegen error so a missing registration
/// fails loudly instead of producing a function with no body.
pub(crate) fn emit_intrinsic_body<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let mangled = function.symbol.mangled();
    let Some(emitter) = emitter_for(mangled) else {
        return Err(LlvmError::Codegen(format!(
            "unknown intrinsic `{mangled}`: no LLVM emitter registered",
        )));
    };
    emitter(ctx, function, llvm_function)
}

fn emitter_for<'ctx>(symbol: &str) -> Option<IntrinsicEmitter<'ctx>> {
    match symbol {
        "Global.print" => Some(emit_global_print),
        _ => None,
    }
}
