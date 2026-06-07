//! `IRInstruction::Clone` emission — the acquisition half of the
//! value-semantics rc glue. Mirrors [`super::locals::emit_drop_value`]:
//! both are value-keyed, type-dispatched, and bottom out in the
//! runtime rc primitives.
//!
//! Three buckets, keyed on the static [`IRType`]:
//!
//! - **Leaf heap** (`String` / `Binary` / `Bits`): a refcount
//!   increment. Cloning a value-semantics heap value shares the
//!   immutable block rather than deep-copying it: `dest` re-binds the
//!   *same* payload pointer and the block's rc is bumped via
//!   [`declare_rc_inc_extern`] on its base (`payload - HEADER_BYTES`).
//!   The matching `Drop` decrements, freeing at zero.
//! - **Copy leaves** (`Bool`, the int / uint / float families, `Unit`,
//!   raw `CPtr`): a register copy. SSA values are immutable, so `dest`
//!   simply re-binds the source value — no rc, no allocation.
//! - **No-glue aggregates** (`Struct` / `Enum` / `Union` whose every
//!   field is `Copy`): a register copy, exactly like the scalar
//!   leaves. The `elaborate` IR sub-pass rewrites only the
//!   *heap-owning* composites into a `Call` to a synthesized per-type
//!   `clone_T`, so a scalar aggregate is all that survives to here.
//! - **Heap composites** (`List` / `Map` / `Set` / `Indirect`, plus
//!   closure `Function`): unreachable. Collections and boxes always
//!   own heap and are always rewritten to a glue `Call`; closures are
//!   a separate slice. A heap composite reaching here is a lowering
//!   bug (panic loudly rather than silently alias).

use koja_ir::{IRType, ValueId};

use crate::ctx::EmitContext;
use crate::emit::heap_layout::block_base;
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::runtime::declare_rc_inc_extern;

use super::{ValueMap, lookup};

pub(super) fn emit_clone<'ctx>(
    ctx: &EmitContext<'ctx>,
    dest: ValueId,
    source: ValueId,
    ty: &IRType,
    values: &mut ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    let result = match ty {
        // Share the immutable block: bump its rc and alias the same
        // payload pointer. The block base (rc word) is `payload -
        // HEADER_BYTES`; the runtime skips immortal (rodata) blocks.
        IRType::String | IRType::Binary | IRType::Bits => {
            let payload = lookup(values, source)?.into_pointer_value();
            let base = block_base(ctx, payload, &format!("{dest}.block_base"))?;
            let rc_inc = declare_rc_inc_extern(ctx);
            ctx.builder
                .build_call(rc_inc, &[base.into()], &format!("{dest}.rc_inc"))
                .map_err(|e| inkwell_err(format_args!("rc_inc call for `{dest}`"), e))?;
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
        // No-glue aggregates (a struct / enum / union whose every
        // field is `Copy`): a register copy, like the scalar leaves.
        // `elaborate` rewrites only the heap-owning composites into
        // `Call @clone_T`, so any aggregate surviving to here owns no
        // heap and aliasing its immutable SSA value is sound.
        IRType::Enum(_) | IRType::Struct(_) | IRType::Union { .. } => lookup(values, source)?,
        // Collections and boxed `Indirect` always own heap, so they
        // always carry glue and must have been rewritten; closures are
        // a separate slice. Reaching here is a lowering bug.
        IRType::Function { .. }
        | IRType::Indirect(_)
        | IRType::List(_)
        | IRType::Map { .. }
        | IRType::Set(_) => panic!(
            "LLVM emit: composite `IRInstruction::Clone` of type {ty:?} reached the backend — \
             the `elaborate` sub-pass must rewrite it into a `Call @clone_T`",
        ),
    };
    values.insert(dest, result);
    Ok(())
}
