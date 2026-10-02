//! `IRInstruction::DeepCopy` emission, the process-boundary copy.
//! Mirrors [`super::clone::emit_clone`]'s type dispatch, but where
//! clone shares heap blocks with an `rc++`, deep copy produces a
//! value with no storage shared with the source (Koja's rc
//! bookkeeping is unsynchronized, so cross-process sharing is
//! unsound).
//!
//! Buckets, keyed on the static [`IRType`]:
//!
//! - **Leaf heap** (`String` / `Binary` / `Bits`): runtime
//!   `koja_heap_deep_copy`, giving a fresh block, `rc = 1`, bytes
//!   copied (immortal rodata blocks are shared as-is).
//! - **Copy leaves** (`Bool`, the int / uint / float families, `Unit`,
//!   raw `CPtr`) and **no-glue aggregates**: a register copy, exactly
//!   like `Clone`'s.
//! - **Closure** (`Function`): runtime `koja_closure_deep_copy`
//!   dispatches through the env header's `copy_fn` glue
//!   ([`koja_ir::FunctionKind::CopyClosureGlue`]), and the fat pointer
//!   is rebuilt around the fresh env.
//! - **Heap composites** (`List` / `Map` / `Set` / `Indirect` and
//!   heap-owning aggregates): unreachable. The `elaborate` sub-pass
//!   rewrites them into a `Call @deep_copy_T`, so one reaching here is
//!   a lowering bug.

use inkwell::values::{BasicValueEnum, PointerValue};
use koja_ir::{IRType, ValueId};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::runtime::{declare_closure_deep_copy_extern, declare_heap_deep_copy_extern};

use super::{ValueMap, closures, lookup};

/// Emit one `IRInstruction::DeepCopy` and return the independent
/// value.
pub(super) fn emit_deep_copy<'ctx>(
    ctx: &EmitContext<'ctx>,
    source: ValueId,
    ty: &IRType,
    values: &ValueMap<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    Ok(match ty {
        IRType::String | IRType::Binary | IRType::Bits => {
            let payload = lookup(values, source).into_pointer_value();
            let deep_copy = declare_heap_deep_copy_extern(ctx);
            ctx.call_basic(deep_copy, &[payload.into()], "deep_copy")?
        }
        IRType::Bool
        | IRType::CPtr(_)
        | IRType::Float32
        | IRType::Float64
        | IRType::Int8
        | IRType::Int16
        | IRType::Int32
        | IRType::Int64
        | IRType::UInt8
        | IRType::UInt16
        | IRType::UInt32
        | IRType::UInt64
        | IRType::Unit => lookup(values, source),
        // No-glue aggregates own no heap, so the register copy is
        // already physically independent (same reasoning as `Clone`'s).
        IRType::Enum(_) | IRType::Struct(_) | IRType::Tuple(_) | IRType::Union { .. } => {
            lookup(values, source)
        }
        IRType::Function { .. } => {
            let closure_value = lookup(values, source);
            let env = closures::load_closure_env_ptr(ctx, closure_value, "deep_copy")?;
            let deep_copy = declare_closure_deep_copy_extern(ctx);
            let fresh_env = ctx
                .call_basic(deep_copy, &[env.into()], "deep_copy.env")?
                .into_pointer_value();
            rebuild_fat_pointer(ctx, closure_value, fresh_env)?
        }
        IRType::Indirect(_) | IRType::List(_) | IRType::Map { .. } | IRType::Set(_) => panic!(
            "LLVM emit: composite `IRInstruction::DeepCopy` of type {ty:?} reached the backend \
             (the `elaborate` sub-pass must rewrite it into a `Call @deep_copy_T`)",
        ),
    })
}

/// Rebuild a `{fn_ptr, env_ptr}` fat pointer around a freshly-copied
/// env with one `insertvalue` over the original's env field.
fn rebuild_fat_pointer<'ctx>(
    ctx: &EmitContext<'ctx>,
    closure_value: BasicValueEnum<'ctx>,
    fresh_env: PointerValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    ctx.builder
        .build_insert_value(
            closure_value.into_struct_value(),
            fresh_env,
            1,
            "deep_copy.value",
        )
        .or_ice()
        .map(|v| v.into_struct_value().into())
}
