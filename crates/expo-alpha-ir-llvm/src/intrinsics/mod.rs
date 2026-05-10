//! Per-backend dispatch table for `@intrinsic` function bodies.
//! Mirrors the eval interpreter's [`expo_alpha_ir_eval::intrinsics`]
//! shape — each registered intrinsic is keyed by its
//! [`expo_alpha_ir::FunctionKind::Intrinsic`] payload (an
//! [`IRIntrinsicId`] -- a typed enum the lift pass mints from the
//! function's identifier path) and routed via an exhaustive `match`
//! to a hand-written emitter that synthesizes the LLVM body.
//!
//! Adding a new intrinsic: extend [`IRIntrinsicId`] in
//! `expo-alpha-ir`, add a sibling `<name>.rs` module exporting
//! `pub(super) fn emit_<name>`, wire its arm in [`emit_intrinsic_body`],
//! and pin a 1-1 test in `tests/intrinsics.rs`. The exhaustive match
//! makes the wiring step compiler-checked.

use expo_alpha_ir::{BitsMethod, IRFunction, IRIntrinsicId, KernelMethod};
use inkwell::values::FunctionValue;

use crate::ctx::EmitContext;
use crate::error::LlvmError;

mod binary;
mod bitwise;
pub(super) mod cptr;
mod cstring;
mod equality;
mod hash;
mod kernel;
mod list;
mod parse;
mod print;
mod string;

/// Synthesize the body of an `@intrinsic` function. Forwards each
/// variant to its hand-written emitter; each emitter receives the
/// inner enum directly (no string-sniffing).
pub(crate) fn emit_intrinsic_body<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    id: &IRIntrinsicId,
) -> Result<(), LlvmError> {
    match *id {
        IRIntrinsicId::Binary(method) => binary::emit_binary(ctx, function, llvm_function, method),
        IRIntrinsicId::Bits(BitsMethod::ToBinary) => {
            binary::emit_bits(ctx, function, llvm_function, BitsMethod::ToBinary)
        }
        IRIntrinsicId::Bitwise { ty, op } => {
            bitwise::emit_bitwise(ctx, function, llvm_function, ty, op)
        }
        IRIntrinsicId::CPtr(method) => cptr::emit_cptr(ctx, function, llvm_function, method),
        IRIntrinsicId::CString(_) => cstring::emit_to_string(ctx, function, llvm_function),
        IRIntrinsicId::Equality(impl_) => equality::emit_eq(ctx, function, llvm_function, impl_),
        IRIntrinsicId::Hash(impl_) => hash::emit_hash(ctx, function, llvm_function, impl_),
        IRIntrinsicId::Kernel(KernelMethod::Panic) => {
            kernel::emit_panic(ctx, function, llvm_function)
        }
        IRIntrinsicId::List(method) => list::emit_list(ctx, function, llvm_function, method),
        IRIntrinsicId::Parse(target) => parse::emit_parse(ctx, function, llvm_function, target),
        IRIntrinsicId::Print => print::emit_global_print(ctx, function, llvm_function),
        IRIntrinsicId::String(method) => string::emit_string(ctx, function, llvm_function, method),
    }
}
