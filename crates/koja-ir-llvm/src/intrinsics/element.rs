//! Canonical per-element *acquire* / *release* under value semantics,
//! shared by the two places that walk a collection's backing buffer:
//! the clone / drop glue bodies ([`crate::emit::collection_glue`]) and
//! the copy-on-write mutators that `memcpy` a buffer before writing
//! (`List` / hashtable intrinsics).
//!
//! Acquiring an element makes a freshly-copied slot own an independent
//! reference; releasing one hands that reference back; deep-copying
//! one severs every share for a process-boundary hand-off:
//!
//! - **heap leaf** (`String` / `Binary` / `Bits`): `rc++` / `rc--` on
//!   the payload block (the pointer is shared, only the count moves);
//!   deep copy swaps in a fresh block via `koja_heap_deep_copy`.
//! - **closure** (`Function`): `rc++` / `koja_closure_rc_dec` on the
//!   fat pointer's env block; deep copy swaps in a fresh env via
//!   `koja_closure_deep_copy`.
//! - **heap composite** carrying glue (`clone_T` / `deep_copy_T`
//!   declared): recurse — acquire / deep copy overwrite the slot with
//!   the glue's result, release calls `drop_T`.
//! - **scalar / no-glue aggregate**: nothing; the `memcpy` already
//!   produced an independent value.
//!
//! The slot forms operate in place on a pointer into the buffer; the
//! buffer forms wrap them in a `0..count` walk for the contiguous
//! element arrays both `List` and the hashtable entry buffer use.

use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use koja_ir::IRType;
use koja_ir::mangling::{clone_glue_symbol, deep_copy_glue_symbol, drop_glue_symbol};

use crate::ctx::EmitContext;
use crate::emit::closures::load_closure_env_ptr;
use crate::emit::heap_layout::{block_base, is_heap_leaf};
use crate::error::{IceExt, LlvmError};
use crate::runtime::{
    declare_closure_deep_copy_extern, declare_closure_rc_dec_extern, declare_heap_deep_copy_extern,
    declare_rc_dec_extern, declare_rc_inc_extern,
};
use crate::types::{closure_fat_ptr_type, ir_basic_type};

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

/// Acquire the element at `slot` (a pointer into a freshly-copied
/// buffer): bump a heap leaf's / closure env's rc, or overwrite the
/// slot with a deep clone for a composite. Scalars need nothing — the
/// `memcpy` already copied them.
pub(crate) fn acquire_in_slot<'ctx>(
    ctx: &EmitContext<'ctx>,
    element: &IRType,
    slot: PointerValue<'ctx>,
) -> Result<(), LlvmError> {
    if is_heap_leaf(element) {
        let payload = load_pointer(ctx, slot, "elem")?;
        let base = block_base(ctx, payload, "elem.block_base")?;
        let rc_inc = declare_rc_inc_extern(ctx);
        ctx.builder
            .build_call(rc_inc, &[base.into()], "elem.rc_inc")
            .or_ice()
            .map(|_| ())
    } else if matches!(element, IRType::Function { .. }) {
        let env = load_env_from_slot(ctx, slot)?;
        let rc_inc = declare_rc_inc_extern(ctx);
        ctx.builder
            .build_call(rc_inc, &[env.into()], "elem.env_rc_inc")
            .or_ice()
            .map(|_| ())
    } else if let Some(clone_glue) = ctx.declared_function(&clone_glue_symbol(element)) {
        copy_slot_through_glue(ctx, element, slot, clone_glue, "clone")
    } else {
        Ok(())
    }
}

/// Deep-copy the element at `slot` (a pointer into a freshly-copied
/// buffer): swap a heap leaf for a fresh block, a closure for a fresh
/// env, or overwrite the slot through `deep_copy_T` for a composite.
/// Scalars need nothing — the `memcpy` already copied them. The
/// process-boundary analog of [`acquire_in_slot`].
pub(crate) fn deep_copy_in_slot<'ctx>(
    ctx: &EmitContext<'ctx>,
    element: &IRType,
    slot: PointerValue<'ctx>,
) -> Result<(), LlvmError> {
    if is_heap_leaf(element) {
        let payload = load_pointer(ctx, slot, "elem")?;
        let deep_copy = declare_heap_deep_copy_extern(ctx);
        let copy = ctx.call_basic(deep_copy, &[payload.into()], "elem.deep_copy")?;
        ctx.builder.build_store(slot, copy).or_ice().map(|_| ())
    } else if matches!(element, IRType::Function { .. }) {
        let env_slot = closure_env_slot(ctx, slot)?;
        let env = load_pointer(ctx, env_slot, "elem.env")?;
        let deep_copy = declare_closure_deep_copy_extern(ctx);
        let copy = ctx.call_basic(deep_copy, &[env.into()], "elem.env_deep_copy")?;
        ctx.builder.build_store(env_slot, copy).or_ice().map(|_| ())
    } else if let Some(deep_copy_glue) = ctx.declared_function(&deep_copy_glue_symbol(element)) {
        copy_slot_through_glue(ctx, element, slot, deep_copy_glue, "deep_copy")
    } else {
        Ok(())
    }
}

/// Release the element at `slot`: rc-decrement a heap leaf / closure
/// env, recurse into `drop_T` for a composite, or do nothing for a
/// scalar.
pub(crate) fn release_in_slot<'ctx>(
    ctx: &EmitContext<'ctx>,
    element: &IRType,
    slot: PointerValue<'ctx>,
) -> Result<(), LlvmError> {
    if is_heap_leaf(element) {
        let payload = load_pointer(ctx, slot, "elem")?;
        let base = block_base(ctx, payload, "elem.block_base")?;
        let rc_dec = declare_rc_dec_extern(ctx);
        ctx.builder
            .build_call(rc_dec, &[base.into()], "elem.rc_dec")
            .or_ice()
            .map(|_| ())
    } else if matches!(element, IRType::Function { .. }) {
        let env = load_env_from_slot(ctx, slot)?;
        let rc_dec = declare_closure_rc_dec_extern(ctx);
        ctx.builder
            .build_call(rc_dec, &[env.into()], "elem.env_rc_dec")
            .or_ice()
            .map(|_| ())
    } else if let Some(drop_glue) = ctx.declared_function(&drop_glue_symbol(element)) {
        let element_ty = ir_basic_type(ctx, element)?;
        let value = ctx.builder.build_load(element_ty, slot, "elem").or_ice()?;
        ctx.builder
            .build_call(drop_glue, &[value.into()], "elem.drop")
            .or_ice()
            .map(|_| ())
    } else {
        Ok(())
    }
}

/// Load the element at `slot`, pass it through `glue` (`clone_T` /
/// `deep_copy_T`), and store the result back — the shared composite
/// path of [`acquire_in_slot`] and [`deep_copy_in_slot`].
fn copy_slot_through_glue<'ctx>(
    ctx: &EmitContext<'ctx>,
    element: &IRType,
    slot: PointerValue<'ctx>,
    glue: FunctionValue<'ctx>,
    label: &str,
) -> Result<(), LlvmError> {
    let element_ty = ir_basic_type(ctx, element)?;
    let original = ctx.builder.build_load(element_ty, slot, "elem").or_ice()?;
    let copied = ctx.call_basic(glue, &[original.into()], &format!("elem.{label}"))?;
    ctx.builder.build_store(slot, copied).or_ice().map(|_| ())
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

/// Load the env pointer of the closure fat pointer stored at `slot`.
fn load_env_from_slot<'ctx>(
    ctx: &EmitContext<'ctx>,
    slot: PointerValue<'ctx>,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let env_slot = closure_env_slot(ctx, slot)?;
    load_pointer(ctx, env_slot, "elem.env")
}

/// Acquire every element in `buf[0..count]` (a contiguous element
/// array). [`acquire_in_slot`] is a no-op for scalars, so this whole
/// walk is skipped when the element owns no heap.
pub(crate) fn acquire_buffer<'ctx>(
    ctx: &EmitContext<'ctx>,
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
        let slot = element_slot(ctx, buf, index, element_size)?;
        acquire_in_slot(ctx, element, slot)
    })
}

/// Deep-copy every element in `buf[0..count]`, the process-boundary
/// analog of [`acquire_buffer`].
pub(crate) fn deep_copy_buffer<'ctx>(
    ctx: &EmitContext<'ctx>,
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
        let slot = element_slot(ctx, buf, index, element_size)?;
        deep_copy_in_slot(ctx, element, slot)
    })
}

/// Release every element in `buf[0..count]`, the drop analog of
/// [`acquire_buffer`].
pub(crate) fn release_buffer<'ctx>(
    ctx: &EmitContext<'ctx>,
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
        let slot = element_slot(ctx, buf, index, element_size)?;
        release_in_slot(ctx, element, slot)
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
    unsafe {
        ctx.builder
            .build_gep(i8_ty, buf, &[offset], "elem.ptr")
            .or_ice()
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
