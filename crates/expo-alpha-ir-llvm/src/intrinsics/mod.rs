//! Per-backend dispatch table for `@intrinsic` function bodies.
//! Mirrors the eval interpreter's [`expo_alpha_ir_eval::intrinsics`]
//! shape — each registered intrinsic is keyed by its
//! [`expo_alpha_ir::FunctionKind::Intrinsic`] `id` (a stable
//! `Type.method` string) and routed to a hand-written emitter that
//! synthesizes the LLVM body.
//!
//! Adding a new intrinsic: drop a sibling `<name>.rs` module
//! exporting `pub(super) fn emit_<name>`, register it in
//! [`emitter_for`], and pin a 1-1 test in
//! `tests/intrinsics.rs`.

use expo_alpha_ir::IRFunction;
use inkwell::values::FunctionValue;

use crate::ctx::EmitContext;
use crate::error::LlvmError;

mod bitwise;
mod print;

use bitwise::{emit_bitwise, op_from_id};
use print::emit_global_print;

/// Function pointer type for an intrinsic's LLVM emitter. The
/// emitter receives the [`EmitContext`], the IR function (for params /
/// return type), the already-declared `FunctionValue` whose body
/// it should fill in, and the dispatch [`id`] (so families of
/// intrinsics like `Bitwise`'s 48-cell table can share one emitter
/// and branch on the trailing `.band`/`.bsl`/...).
type IntrinsicEmitter<'ctx> =
    fn(&EmitContext<'ctx>, &IRFunction, FunctionValue<'ctx>, &str) -> Result<(), LlvmError>;

/// Synthesize the body of an `@intrinsic` function. Looks `id` up
/// in the dispatch table and forwards to the registered emitter;
/// unknown keys surface as an explicit "unknown intrinsic" codegen
/// error so a missing registration fails loudly instead of producing
/// a function with no body. `id` is the
/// [`expo_alpha_ir::FunctionKind::Intrinsic`] payload — the
/// caller (`define_function`) destructures the kind and passes it
/// in.
pub(crate) fn emit_intrinsic_body<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    id: &str,
) -> Result<(), LlvmError> {
    let Some(emitter) = emitter_for(id) else {
        return Err(LlvmError::Codegen(format!(
            "unknown intrinsic `{id}` (symbol `{}`): no LLVM emitter registered",
            function.symbol,
        )));
    };
    emitter(ctx, function, llvm_function, id)
}

fn emitter_for<'ctx>(id: &str) -> Option<IntrinsicEmitter<'ctx>> {
    if id == "print" {
        return Some(emit_global_print);
    }
    // 48-cell `Bitwise` family: `Int.band`, `UInt8.bsl`, ...
    // Routes here when the trailing segment is one of the six
    // ops; the emitter branches on the parsed `Op` to issue the
    // right LLVM instruction.
    if op_from_id(id).is_some() {
        return Some(emit_bitwise);
    }
    None
}
