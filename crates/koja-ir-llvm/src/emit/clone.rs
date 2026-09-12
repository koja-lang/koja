//! `IRInstruction::Clone` emission, the acquisition half of the
//! value-semantics rc glue. Heap leaves, `Indirect` boxes, and
//! closure envs share their block through an `rc++`. Everything else
//! that reaches here is a register copy, because `elaborate` has
//! already rewritten every heap-owning composite into a `Call` to
//! its `clone_T` glue.

use koja_ir::{IRType, ValueId};

use crate::ctx::EmitContext;
use crate::emit::heap_layout::block_base;
use crate::error::{IceExt, LlvmError};
use crate::runtime::declare_rc_inc_extern;

use super::{ValueMap, closures, lookup};

pub(super) fn emit_clone<'ctx>(
    ctx: &EmitContext<'ctx>,
    dest: ValueId,
    source: ValueId,
    ty: &IRType,
    values: &mut ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    let result = match ty {
        IRType::Binary | IRType::Bits | IRType::Indirect(_) | IRType::String => {
            let payload = lookup(values, source)?.into_pointer_value();
            let base = block_base(ctx, payload, &format!("{dest}.block_base"))?;
            let rc_inc = declare_rc_inc_extern(ctx);
            ctx.builder
                .build_call(rc_inc, &[base.into()], &format!("{dest}.rc_inc"))
                .or_ice()?;
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
        | IRType::Unit => lookup(values, source)?,
        IRType::Enum(_) | IRType::Struct(_) | IRType::Tuple(_) | IRType::Union { .. } => {
            lookup(values, source)?
        }
        IRType::Function { .. } => {
            let closure_value = lookup(values, source)?;
            let env_ptr =
                closures::load_closure_env_ptr(ctx, closure_value, &format!("{dest}.clone"))?;
            let rc_inc = declare_rc_inc_extern(ctx);
            ctx.builder
                .build_call(rc_inc, &[env_ptr.into()], &format!("{dest}.env_rc_inc"))
                .or_ice()?;
            closure_value
        }
        IRType::List(_) | IRType::Map { .. } | IRType::Set(_) => panic!(
            "LLVM emit: composite `IRInstruction::Clone` of type {ty:?} reached the backend \
             (the `elaborate` sub-pass must rewrite it into a `Call @clone_T`)",
        ),
    };
    values.insert(dest, result);
    Ok(())
}
