//! `IRInstruction::Clone` emission, the acquisition half of the
//! value-semantics rc glue. Heap leaves, `Indirect` boxes, and
//! closure envs share their block through an `rc++`. Everything else
//! that reaches here is a register copy, because `elaborate` has
//! already rewritten every heap-owning composite into a `Call` to
//! its `clone_T` glue.

use inkwell::values::BasicValueEnum;
use koja_ir::{IRType, ValueId};

use crate::ctx::EmitContext;
use crate::emit::heap_layout::block_base;
use crate::error::LlvmError;
use crate::runtime::declare_rc_inc_extern;

use super::{ValueMap, closures, lookup};

/// Emit one `IRInstruction::Clone` and return the acquired value.
pub(super) fn emit_clone<'ctx>(
    ctx: &EmitContext<'ctx>,
    source: ValueId,
    ty: &IRType,
    values: &ValueMap<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    Ok(match ty {
        IRType::Binary | IRType::Bits | IRType::Indirect(_) | IRType::String => {
            let payload = lookup(values, source).into_pointer_value();
            let base = block_base(ctx, payload, "clone.block_base")?;
            ctx.call_rt_unit(declare_rc_inc_extern, &[base.into()])?;
            payload.into()
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
        IRType::Enum(_) | IRType::Struct(_) | IRType::Tuple(_) | IRType::Union { .. } => {
            lookup(values, source)
        }
        IRType::Function { .. } => {
            let closure_value = lookup(values, source);
            let env_ptr = closures::load_closure_env_ptr(ctx, closure_value, "clone")?;
            ctx.call_rt_unit(declare_rc_inc_extern, &[env_ptr.into()])?;
            closure_value
        }
        IRType::List(_) | IRType::Map { .. } | IRType::Set(_) => panic!(
            "LLVM emit: composite `IRInstruction::Clone` of type {ty:?} reached the backend \
             (the `elaborate` sub-pass must rewrite it into a `Call @clone_T`)",
        ),
    })
}
