//! Canonical per-element *acquire* / *release* under value semantics,
//! shared by the two places that walk a collection's backing buffer:
//! the clone / drop glue bodies ([`crate::emit::collection_glue`]) and
//! the copy-on-write mutators that `memcpy` a buffer before writing
//! (`List` / hashtable intrinsics).
//!
//! Acquiring an element makes a freshly-copied slot own an independent
//! reference; releasing one hands that reference back:
//!
//! - **heap leaf** (`String` / `Binary` / `Bits`): `rc++` / `rc--` on
//!   the payload block — the pointer is shared, only the count moves.
//! - **heap composite** carrying glue (`clone_T` declared): recurse —
//!   acquire deep-clones into the slot, release calls `drop_T`.
//! - **scalar / no-glue aggregate**: nothing; the `memcpy` already
//!   produced an independent value.
//!
//! The slot forms operate in place on a pointer into the buffer; the
//! buffer forms wrap them in a `0..count` walk for the contiguous
//! element arrays both `List` and the hashtable entry buffer use.

use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use koja_ir::mangling::{clone_glue_symbol, drop_glue_symbol};
use koja_ir::{IRSymbol, IRType};

use crate::ctx::EmitContext;
use crate::emit::heap_layout::{block_base, is_heap_leaf};
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::runtime::{declare_rc_dec_extern, declare_rc_inc_extern};
use crate::types::ir_basic_type;

/// Acquire a collection element held as an SSA value — an insert
/// store-in (`List.append` / `List.replace_at`, hashtable insert) or a
/// hand-out (`List.get` / `List.pop`). Returns the value the caller
/// should store or return: a heap leaf passes through after `rc++`, a
/// composite becomes an independent deep clone via its `clone_T` glue,
/// and a scalar (or no-glue aggregate) passes through untouched. This
/// is the value-form counterpart to [`acquire_in_slot`], which works
/// on a buffer slot in place.
pub(crate) fn acquire_value<'ctx>(
    ctx: &EmitContext<'ctx>,
    site: &IRSymbol,
    element: &IRType,
    value: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    if is_heap_leaf(element) {
        let base = block_base(ctx, value.into_pointer_value(), "elem.block_base")?;
        let rc_inc = declare_rc_inc_extern(ctx);
        ctx.builder
            .build_call(rc_inc, &[base.into()], "elem.rc_inc")
            .map_err(|e| inkwell_err(format_args!("element rc_inc for `{}`", site), e))?;
        Ok(value)
    } else if let Some(clone_glue) = ctx.declared_function(&clone_glue_symbol(element)) {
        ctx.builder
            .build_call(clone_glue, &[value.into()], "elem.clone")
            .map_err(|e| inkwell_err(format_args!("element clone for `{}`", site), e))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| LlvmError::Codegen(format!("clone glue returned void for `{}`", site)))
    } else {
        Ok(value)
    }
}

/// Acquire the element at `slot` (a pointer into a freshly-copied
/// buffer): bump a heap leaf's rc, or overwrite the slot with a deep
/// clone for a composite. Scalars need nothing — the `memcpy` already
/// copied them.
pub(crate) fn acquire_in_slot<'ctx>(
    ctx: &EmitContext<'ctx>,
    site: &IRSymbol,
    element: &IRType,
    slot: PointerValue<'ctx>,
) -> Result<(), LlvmError> {
    if is_heap_leaf(element) {
        let payload = load_pointer(ctx, site, slot, "elem")?;
        let base = block_base(ctx, payload, "elem.block_base")?;
        let rc_inc = declare_rc_inc_extern(ctx);
        ctx.builder
            .build_call(rc_inc, &[base.into()], "elem.rc_inc")
            .map(|_| ())
            .map_err(|e| inkwell_err(format_args!("element rc_inc for `{}`", site), e))
    } else if let Some(clone_glue) = ctx.declared_function(&clone_glue_symbol(element)) {
        let element_ty = ir_basic_type(ctx, element)?;
        let original = ctx
            .builder
            .build_load(element_ty, slot, "elem")
            .map_err(|e| inkwell_err(format_args!("element load for `{}`", site), e))?;
        let cloned = ctx
            .builder
            .build_call(clone_glue, &[original.into()], "elem.clone")
            .map_err(|e| inkwell_err(format_args!("element clone for `{}`", site), e))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| {
                LlvmError::Codegen(format!("clone glue returned void for `{}`", site))
            })?;
        ctx.builder
            .build_store(slot, cloned)
            .map(|_| ())
            .map_err(|e| inkwell_err(format_args!("element store for `{}`", site), e))
    } else {
        Ok(())
    }
}

/// Release the element at `slot`: rc-decrement a heap leaf, recurse
/// into `drop_T` for a composite, or do nothing for a scalar.
pub(crate) fn release_in_slot<'ctx>(
    ctx: &EmitContext<'ctx>,
    site: &IRSymbol,
    element: &IRType,
    slot: PointerValue<'ctx>,
) -> Result<(), LlvmError> {
    if is_heap_leaf(element) {
        let payload = load_pointer(ctx, site, slot, "elem")?;
        let base = block_base(ctx, payload, "elem.block_base")?;
        let rc_dec = declare_rc_dec_extern(ctx);
        ctx.builder
            .build_call(rc_dec, &[base.into()], "elem.rc_dec")
            .map(|_| ())
            .map_err(|e| inkwell_err(format_args!("element rc_dec for `{}`", site), e))
    } else if let Some(drop_glue) = ctx.declared_function(&drop_glue_symbol(element)) {
        let element_ty = ir_basic_type(ctx, element)?;
        let value = ctx
            .builder
            .build_load(element_ty, slot, "elem")
            .map_err(|e| inkwell_err(format_args!("element load for `{}`", site), e))?;
        ctx.builder
            .build_call(drop_glue, &[value.into()], "elem.drop")
            .map(|_| ())
            .map_err(|e| inkwell_err(format_args!("element drop for `{}`", site), e))
    } else {
        Ok(())
    }
}

/// Acquire every element in `buf[0..count]` (a contiguous element
/// array). [`acquire_in_slot`] is a no-op for scalars, so this whole
/// walk is skipped when the element owns no heap.
#[allow(clippy::too_many_arguments)]
pub(crate) fn acquire_buffer<'ctx>(
    ctx: &EmitContext<'ctx>,
    site: &IRSymbol,
    llvm_function: FunctionValue<'ctx>,
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
        let slot = element_slot(ctx, site, buf, index, element_size)?;
        acquire_in_slot(ctx, site, element, slot)
    })
}

/// Release every element in `buf[0..count]`, the drop analog of
/// [`acquire_buffer`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn release_buffer<'ctx>(
    ctx: &EmitContext<'ctx>,
    site: &IRSymbol,
    llvm_function: FunctionValue<'ctx>,
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
        let slot = element_slot(ctx, site, buf, index, element_size)?;
        release_in_slot(ctx, site, element, slot)
    })
}

/// Whether `element` carries any acquire / release work: a heap leaf
/// (rc) or a composite with declared glue. Scalars and no-glue
/// aggregates answer `false`, letting [`acquire_buffer`] /
/// [`release_buffer`] skip the walk entirely.
fn owns_heap<'ctx>(ctx: &EmitContext<'ctx>, element: &IRType) -> bool {
    is_heap_leaf(element) || ctx.declared_function(&clone_glue_symbol(element)).is_some()
}

/// Pointer to element `index` in a byte-addressed buffer.
pub(crate) fn element_slot<'ctx>(
    ctx: &EmitContext<'ctx>,
    site: &IRSymbol,
    buf: PointerValue<'ctx>,
    index: IntValue<'ctx>,
    element_size: IntValue<'ctx>,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let offset = ctx
        .builder
        .build_int_mul(index, element_size, "elem.off")
        .map_err(|e| inkwell_err(format_args!("element offset for `{}`", site), e))?;
    unsafe {
        ctx.builder
            .build_gep(i8_ty, buf, &[offset], "elem.ptr")
            .map_err(|e| inkwell_err(format_args!("element GEP for `{}`", site), e))
    }
}

/// Emit a `for index in 0..count` loop whose straight-line `body` is
/// generated once into the loop body block. The body must not branch
/// (it emits into and leaves control in the body block); the helper
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
        .map_err(|e| inkwell_err(format_args!("{label} loop counter init"), e))?;
    let head = ctx
        .context
        .append_basic_block(llvm_function, &format!("{label}.head"));
    let body_block = ctx
        .context
        .append_basic_block(llvm_function, &format!("{label}.body"));
    let exit = ctx
        .context
        .append_basic_block(llvm_function, &format!("{label}.exit"));

    ctx.builder
        .build_unconditional_branch(head)
        .map_err(|e| inkwell_err(format_args!("{label} loop entry branch"), e))?;
    ctx.builder.position_at_end(head);
    let index = ctx
        .builder
        .build_load(i64_ty, counter, &format!("{label}.idx"))
        .map_err(|e| inkwell_err(format_args!("{label} loop index load"), e))?
        .into_int_value();
    let in_range = ctx
        .builder
        .build_int_compare(IntPredicate::ULT, index, count, &format!("{label}.cmp"))
        .map_err(|e| inkwell_err(format_args!("{label} loop guard"), e))?;
    ctx.builder
        .build_conditional_branch(in_range, body_block, exit)
        .map_err(|e| inkwell_err(format_args!("{label} loop branch"), e))?;

    ctx.builder.position_at_end(body_block);
    body(ctx, index)?;
    let next = ctx
        .builder
        .build_int_add(index, i64_ty.const_int(1, false), &format!("{label}.inc"))
        .map_err(|e| inkwell_err(format_args!("{label} loop increment"), e))?;
    ctx.builder
        .build_store(counter, next)
        .map_err(|e| inkwell_err(format_args!("{label} loop counter store"), e))?;
    ctx.builder
        .build_unconditional_branch(head)
        .map_err(|e| inkwell_err(format_args!("{label} loop back-edge"), e))?;

    ctx.builder.position_at_end(exit);
    Ok(())
}

fn load_pointer<'ctx>(
    ctx: &EmitContext<'ctx>,
    site: &IRSymbol,
    slot: PointerValue<'ctx>,
    name: &str,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    ctx.builder
        .build_load(ptr_ty, slot, name)
        .map(|v| v.into_pointer_value())
        .map_err(|e| inkwell_err(format_args!("element ptr load for `{}`", site), e))
}
