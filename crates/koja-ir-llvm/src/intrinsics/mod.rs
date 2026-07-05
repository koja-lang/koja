//! Per-backend dispatch table for `@intrinsic` function bodies.
//! Mirrors the eval interpreter's [`koja_ir_eval::intrinsics`]
//! shape: each registered intrinsic is keyed by its
//! [`koja_ir::FunctionKind::Intrinsic`] payload (an
//! [`IRIntrinsicId`], a typed enum the lift pass mints from the
//! function's identifier path) and routed via an exhaustive `match`
//! to a hand-written emitter that synthesizes the LLVM body.
//!
//! Adding a new intrinsic: extend [`IRIntrinsicId`] in
//! `koja-ir`, add a sibling `<name>.rs` module exporting
//! `pub(super) fn emit_<name>`, wire its arm in [`emit_intrinsic_body`],
//! and pin a 1-1 test in `tests/intrinsics.rs`. The exhaustive match
//! makes the wiring step compiler-checked.

use inkwell::values::FunctionValue;
use koja_ir::{IRFunction, IRIntrinsicId, KernelMethod};

use crate::ctx::EmitContext;
use crate::error::LlvmError;

mod binary;
mod bitwise;
pub(super) mod cptr;
mod cstring;
mod debug;
pub(crate) mod element;
mod equality;
mod hash;
mod hashtable;
pub(crate) mod heap_payload;
mod kernel;
mod list;
mod map;
mod numeric;
mod parse;
mod print;
pub(crate) mod process;
mod set;
mod socket;
mod string;

/// The `0..capacity` occupied-bucket walk, re-exported so the collection
/// glue emitter ([`crate::emit::collection_glue`]) iterates `Map` /
/// `Set` buffers by the exact convention the hashtable intrinsics write.
pub(crate) use hashtable::occupied_loop;

/// Synthesize the body of an `@intrinsic` function. Forwards each
/// variant to its hand-written emitter. Each emitter receives the
/// inner enum directly (no string-sniffing).
pub(crate) fn emit_intrinsic_body<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    id: &IRIntrinsicId,
) -> Result<(), LlvmError> {
    match *id {
        IRIntrinsicId::Binary(method) => binary::emit_binary(ctx, function, llvm_function, method),
        IRIntrinsicId::Bits(method) => binary::emit_bits(ctx, function, llvm_function, method),
        IRIntrinsicId::Bitwise { ty, op } => {
            bitwise::emit_bitwise(ctx, function, llvm_function, ty, op)
        }
        IRIntrinsicId::CPtr(method) => cptr::emit_cptr(ctx, function, llvm_function, method),
        IRIntrinsicId::CString(_) => cstring::emit_to_string(ctx, function, llvm_function),
        IRIntrinsicId::Debug(impl_) => debug::emit_format(ctx, function, llvm_function, impl_),
        IRIntrinsicId::Equality(impl_) => equality::emit_eq(ctx, function, llvm_function, impl_),
        IRIntrinsicId::Hash(impl_) => hash::emit_hash(ctx, function, llvm_function, impl_),
        IRIntrinsicId::Kernel(KernelMethod::Panic) => {
            kernel::emit_panic(ctx, function, llvm_function)
        }
        IRIntrinsicId::List(method) => list::emit_list(ctx, function, llvm_function, method),
        IRIntrinsicId::Map(method) => map::emit_map(ctx, function, llvm_function, method),
        IRIntrinsicId::NumericConvert(convert) => {
            numeric::emit_numeric_convert(ctx, function, llvm_function, convert)
        }
        IRIntrinsicId::Parse(target) => parse::emit_parse(ctx, function, llvm_function, target),
        IRIntrinsicId::Print => print::emit_global_print(ctx, function, llvm_function),
        IRIntrinsicId::Process(method) => {
            process::emit_process(ctx, function, llvm_function, method)
        }
        IRIntrinsicId::Ref(method) => process::emit_ref(ctx, function, llvm_function, method),
        IRIntrinsicId::ReplyTo(method) => {
            process::emit_reply_to(ctx, function, llvm_function, method)
        }
        IRIntrinsicId::Set(method) => set::emit_set(ctx, function, llvm_function, method),
        IRIntrinsicId::Socket(method) => socket::emit_socket(ctx, function, llvm_function, method),
        IRIntrinsicId::String(method) => string::emit_string(ctx, function, llvm_function, method),
    }
}
