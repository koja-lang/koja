//! Per-backend dispatch table for `@intrinsic` function bodies.
//! Mirrors the eval interpreter's [`koja_ir_eval::intrinsics`]
//! shape — each registered intrinsic is keyed by its
//! [`koja_ir::FunctionKind::Intrinsic`] payload (an
//! [`IRIntrinsicId`] -- a typed enum the lift pass mints from the
//! function's identifier path) and routed via an exhaustive `match`
//! to a hand-written emitter that synthesizes the LLVM body.
//!
//! Adding a new intrinsic: extend [`IRIntrinsicId`] in
//! `koja-ir`, add a sibling `<name>.rs` module exporting
//! `pub(super) fn emit_<name>`, wire its arm in [`emit_intrinsic_body`],
//! and pin a 1-1 test in `tests/intrinsics.rs`. The exhaustive match
//! makes the wiring step compiler-checked.

use inkwell::values::{BasicValueEnum, FunctionValue};
use koja_ir::{IRFunction, IRIntrinsicId, IRType, KernelMethod};

use crate::ctx::EmitContext;
use crate::emit::heap_layout::{block_base, is_heap_leaf};
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::runtime::declare_rc_inc_extern;

mod binary;
mod bitwise;
pub(super) mod cptr;
mod cstring;
mod debug;
mod equality;
mod hash;
mod hashtable;
pub(crate) mod heap_clone;
mod kernel;
mod list;
mod map;
mod parse;
mod print;
mod process;
mod set;
mod socket;
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
        IRIntrinsicId::Parse(target) => parse::emit_parse(ctx, function, llvm_function, target),
        IRIntrinsicId::Print => print::emit_global_print(ctx, function, llvm_function),
        IRIntrinsicId::Ref(method) => process::emit_ref(ctx, function, llvm_function, method),
        IRIntrinsicId::ReplyTo(method) => {
            process::emit_reply_to(ctx, function, llvm_function, method)
        }
        IRIntrinsicId::Set(method) => set::emit_set(ctx, function, llvm_function, method),
        IRIntrinsicId::Socket(method) => socket::emit_socket(ctx, function, llvm_function, method),
        IRIntrinsicId::String(method) => string::emit_string(ctx, function, llvm_function, method),
    }
}

/// Acquire a collection element under value semantics: when `elem_ty`
/// is an rc-managed heap leaf (`String` / `Binary` / `Bits`), bump its
/// refcount so the container takes — or a freshly handed-out binding
/// receives — an independent reference. A no-op for inline scalar and
/// (for now) composite elements. Hand-written collection intrinsics
/// call this at every store-in (`append`, `replace_at`) and hand-out
/// (`get`, `pop`) site, standing in for the clone glue that callee
/// parameter promotion supplies to ordinary functions. `value` is the
/// element's SSA value — a payload pointer for heap leaves.
pub(super) fn acquire_element<'ctx>(
    ctx: &EmitContext<'ctx>,
    elem_ty: &IRType,
    value: BasicValueEnum<'ctx>,
    label: &str,
) -> Result<(), LlvmError> {
    if !is_heap_leaf(elem_ty) {
        return Ok(());
    }
    let base = block_base(
        ctx,
        value.into_pointer_value(),
        &format!("{label}.block_base"),
    )?;
    let rc_inc = declare_rc_inc_extern(ctx);
    ctx.builder
        .build_call(rc_inc, &[base.into()], &format!("{label}.rc_inc"))
        .map(|_| ())
        .map_err(|e| inkwell_err(format_args!("element rc_inc for `{label}`"), e))
}
