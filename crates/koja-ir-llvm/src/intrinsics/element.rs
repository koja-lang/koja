//! Canonical per-element *acquire* / *release* under value semantics,
//! shared by the two places that walk a collection's backing buffer:
//! the clone / drop glue bodies ([`crate::emit::collection_glue`]) and
//! the copy-on-write mutators that `memcpy` a buffer before writing
//! (`List` / hashtable intrinsics).
//!
//! Acquiring an element makes a freshly-copied slot own an independent
//! reference, releasing one hands that reference back, and deep-copying
//! one severs every share for a process-boundary hand-off:
//!
//! - **heap leaf** (`String` / `Binary` / `Bits`): `rc++` / `rc--` on
//!   the payload block (the pointer is shared, only the count moves).
//!   Deep copy swaps in a fresh block via `koja_heap_deep_copy`.
//! - **closure** (`Function`): `rc++` / `koja_closure_rc_dec` on the
//!   fat pointer's env block. Deep copy swaps in a fresh env via
//!   `koja_closure_deep_copy`.
//! - **heap composite** carrying glue (`clone_T` / `deep_copy_T`
//!   declared): recurse. Acquire / deep copy overwrite the slot with
//!   the glue's result, release calls `drop_T`.
//! - **scalar / no-glue aggregate**: nothing, the `memcpy` already
//!   produced an independent value.
//!
//! [`ElementOp`] names the op. [`apply_in_slot`] runs it in place on
//! a pointer into the buffer. [`walk_buffer`] wraps that in a
//! `0..count` walk for the contiguous element arrays both `List` and
//! the hashtable entry buffer use.

use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use koja_ir::mangling::{clone_glue_symbol, deep_copy_glue_symbol, drop_glue_symbol};
use koja_ir::{IRSymbol, IRType};

use crate::ctx::EmitContext;
use crate::emit::closures::load_closure_env_ptr;
use crate::emit::heap_layout::{block_base, is_heap_leaf};
use crate::error::{IceExt, LlvmError};
use crate::runtime::{
    declare_closure_deep_copy_extern, declare_closure_rc_dec_extern, declare_heap_deep_copy_extern,
    declare_rc_dec_extern, declare_rc_inc_extern,
};
use crate::types::{closure_fat_ptr_type, ir_basic_type};

/// Acquire a collection element held as an SSA value: an insert
/// store-in (`List.append` / `List.replace_at`, hashtable insert) or a
/// hand-out (`List.get` / `List.pop`). Returns the value the caller
/// should store or return: a heap leaf passes through after `rc++`, a
/// composite becomes an independent deep clone via its `clone_T` glue,
/// and a scalar (or no-glue aggregate) passes through untouched. This
/// is the value-form counterpart to [`apply_in_slot`] with
/// [`ElementOp::Acquire`], which works on a buffer slot in place.
pub(crate) fn acquire_value<'ctx>(
    ctx: &EmitContext<'ctx>,
    element: &IRType,
    value: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    if is_heap_leaf(element) {
        let base = block_base(ctx, value.into_pointer_value(), "elem.block_base")?;
        let rc_inc = declare_rc_inc_extern(ctx);
        ctx.builder
            .build_call(rc_inc, &[base.into()], "elem.rc_inc")
            .or_ice()?;
        Ok(value)
    } else if matches!(element, IRType::Function { .. }) {
        let env = load_closure_env_ptr(ctx, value, "elem.closure_acquire")?;
        let rc_inc = declare_rc_inc_extern(ctx);
        ctx.builder
            .build_call(rc_inc, &[env.into()], "elem.env_rc_inc")
            .or_ice()?;
        Ok(value)
    } else if let Some(clone_glue) = ctx.declared_function(&clone_glue_symbol(element)) {
        ctx.call_basic(clone_glue, &[value.into()], "elem.clone")
    } else {
        Ok(value)
    }
}

/// The ownership op a slot or buffer walk applies to each element.
#[derive(Clone, Copy)]
pub(crate) enum ElementOp {
    /// Make the slot own an independent reference, `rc++` on a heap
    /// leaf or closure env and `clone_T` for a composite.
    Acquire,
    /// Sever every share with a fresh block or env, or `deep_copy_T`
    /// for a composite. The process-boundary analog of `Acquire`.
    DeepCopy,
    /// Hand the reference back, `rc--` on a heap leaf or closure env
    /// and `drop_T` for a composite.
    Release,
}

/// Apply `op` to the element at `slot`, a pointer into a buffer.
/// Scalars and no-glue aggregates need nothing, the `memcpy` that
/// produced the buffer already copied them.
pub(crate) fn apply_in_slot<'ctx>(
    ctx: &EmitContext<'ctx>,
    op: ElementOp,
    element: &IRType,
    slot: PointerValue<'ctx>,
) -> Result<(), LlvmError> {
    if is_heap_leaf(element) {
        let payload = load_pointer(ctx, slot, "elem")?;
        match op {
            ElementOp::Acquire => {
                let base = block_base(ctx, payload, "elem.block_base")?;
                ctx.call_rt_unit(declare_rc_inc_extern, &[base.into()])
            }
            ElementOp::DeepCopy => {
                let deep_copy = declare_heap_deep_copy_extern(ctx);
                let copy = ctx.call_basic(deep_copy, &[payload.into()], "elem.deep_copy")?;
                ctx.builder.build_store(slot, copy).or_ice().map(|_| ())
            }
            ElementOp::Release => {
                let base = block_base(ctx, payload, "elem.block_base")?;
                ctx.call_rt_unit(declare_rc_dec_extern, &[base.into()])
            }
        }
    } else if matches!(element, IRType::Function { .. }) {
        let env_slot = closure_env_slot(ctx, slot)?;
        let env = load_pointer(ctx, env_slot, "elem.env")?;
        match op {
            ElementOp::Acquire => ctx.call_rt_unit(declare_rc_inc_extern, &[env.into()]),
            ElementOp::DeepCopy => {
                let deep_copy = declare_closure_deep_copy_extern(ctx);
                let copy = ctx.call_basic(deep_copy, &[env.into()], "elem.env_deep_copy")?;
                ctx.builder.build_store(env_slot, copy).or_ice().map(|_| ())
            }
            ElementOp::Release => ctx.call_rt_unit(declare_closure_rc_dec_extern, &[env.into()]),
        }
    } else if let Some(glue) = ctx.declared_function(&glue_symbol(op, element)) {
        let element_ty = ir_basic_type(ctx, element)?;
        let value = ctx.builder.build_load(element_ty, slot, "elem").or_ice()?;
        match op {
            ElementOp::Acquire | ElementOp::DeepCopy => {
                let copied = ctx.call_basic(glue, &[value.into()], glue_label(op))?;
                ctx.builder.build_store(slot, copied).or_ice().map(|_| ())
            }
            ElementOp::Release => ctx
                .builder
                .build_call(glue, &[value.into()], glue_label(op))
                .or_ice()
                .map(|_| ()),
        }
    } else {
        Ok(())
    }
}

/// The composite glue `op` routes through for `element`.
fn glue_symbol(op: ElementOp, element: &IRType) -> IRSymbol {
    match op {
        ElementOp::Acquire => clone_glue_symbol(element),
        ElementOp::DeepCopy => deep_copy_glue_symbol(element),
        ElementOp::Release => drop_glue_symbol(element),
    }
}

/// SSA name for the composite glue call `op` emits.
fn glue_label(op: ElementOp) -> &'static str {
    match op {
        ElementOp::Acquire => "elem.clone",
        ElementOp::DeepCopy => "elem.deep_copy",
        ElementOp::Release => "elem.drop",
    }
}

/// GEP to the `env_ptr` field of the closure fat pointer stored at
/// `slot`.
fn closure_env_slot<'ctx>(
    ctx: &EmitContext<'ctx>,
    slot: PointerValue<'ctx>,
) -> Result<PointerValue<'ctx>, LlvmError> {
    ctx.builder
        .build_struct_gep(closure_fat_ptr_type(ctx), slot, 1, "elem.env_ptr")
        .or_ice()
}

/// Apply `op` to every element in `buf[0..count]`, a contiguous
/// element array. [`apply_in_slot`] is a no-op for scalars, so the
/// whole walk is skipped when the element owns no heap.
#[allow(clippy::too_many_arguments)]
pub(crate) fn walk_buffer<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    op: ElementOp,
    element: &IRType,
    buf: PointerValue<'ctx>,
    count: IntValue<'ctx>,
    element_size: IntValue<'ctx>,
    label: &str,
) -> Result<(), LlvmError> {
    if !owns_heap(ctx, element) {
        return Ok(());
    }
    index_loop(ctx, llvm_function, count, label, |ctx, index| {
        let slot = element_slot(ctx, buf, index, element_size)?;
        apply_in_slot(ctx, op, element, slot)
    })
}

/// Whether `element` carries any acquire / deep-copy / release work:
/// a heap leaf (rc), a closure (env rc), or a composite with declared
/// glue. Scalars and no-glue aggregates answer `false`, letting the
/// buffer walks skip entirely.
fn owns_heap<'ctx>(ctx: &EmitContext<'ctx>, element: &IRType) -> bool {
    is_heap_leaf(element)
        || matches!(element, IRType::Function { .. })
        || ctx.declared_function(&clone_glue_symbol(element)).is_some()
        || ctx
            .declared_function(&deep_copy_glue_symbol(element))
            .is_some()
}

/// Pointer to element `index` in a byte-addressed buffer.
pub(crate) fn element_slot<'ctx>(
    ctx: &EmitContext<'ctx>,
    buf: PointerValue<'ctx>,
    index: IntValue<'ctx>,
    element_size: IntValue<'ctx>,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let offset = ctx
        .builder
        .build_int_mul(index, element_size, "elem.off")
        .or_ice()?;
    // SAFETY: callers bound `index` by the buffer's element count.
    unsafe {
        ctx.builder
            .build_gep(i8_ty, buf, &[offset], "elem.ptr")
            .or_ice()
    }
}

/// Emit a `for index in 0..count` loop whose straight-line `body` is
/// generated once into the loop body block. The body must not branch
/// (it emits into and leaves control in the body block). The helper
/// owns the counter, the `index < count` guard, and the back-edge.
fn index_loop<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    count: IntValue<'ctx>,
    label: &str,
    body: impl FnOnce(&EmitContext<'ctx>, IntValue<'ctx>) -> Result<(), LlvmError>,
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let counter = ctx.build_entry_alloca(i64_ty, &format!("{label}.i"));
    ctx.builder
        .build_store(counter, i64_ty.const_zero())
        .or_ice()?;
    let head = ctx
        .context
        .append_basic_block(llvm_function, &format!("{label}.head"));
    let body_block = ctx
        .context
        .append_basic_block(llvm_function, &format!("{label}.body"));
    let exit = ctx
        .context
        .append_basic_block(llvm_function, &format!("{label}.exit"));

    ctx.builder.build_unconditional_branch(head).or_ice()?;
    ctx.builder.position_at_end(head);
    let index = ctx
        .builder
        .build_load(i64_ty, counter, &format!("{label}.idx"))
        .or_ice()?
        .into_int_value();
    let in_range = ctx
        .builder
        .build_int_compare(IntPredicate::ULT, index, count, &format!("{label}.cmp"))
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(in_range, body_block, exit)
        .or_ice()?;

    ctx.builder.position_at_end(body_block);
    body(ctx, index)?;
    let next = ctx
        .builder
        .build_int_add(index, i64_ty.const_int(1, false), &format!("{label}.inc"))
        .or_ice()?;
    ctx.builder.build_store(counter, next).or_ice()?;
    ctx.builder.build_unconditional_branch(head).or_ice()?;

    ctx.builder.position_at_end(exit);
    Ok(())
}

fn load_pointer<'ctx>(
    ctx: &EmitContext<'ctx>,
    slot: PointerValue<'ctx>,
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    ctx.builder
        .build_load(ptr_ty, slot, name)
        .or_ice()
        .map(|v| v.into_pointer_value())
}
