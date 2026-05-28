//! `Set.from_list(items)` — walk the list's flat buffer and fold
//! each element into a fresh `Set` via [`call_set_insert_inline`].
//! Lives in its own module because it stitches together pieces from
//! every other submodule ([`build_empty_table`](super::util::build_empty_table),
//! [`emit_resize_if_needed`](super::resize::emit_resize_if_needed),
//! [`emit_insert_probe`](super::insert::emit_insert_probe)) without
//! belonging to any of them.

use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, FunctionValue};
use koja_ir::IRFunction;

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::types::{hashtable_value_type, ir_basic_type, list_value_type};

use super::insert::emit_insert_probe;
use super::resize::emit_resize_if_needed;
use super::util::{
    TableSnapshot, build_empty_table, build_table_struct, codegen_err, entry_pointer, extract_int,
    extract_pointer, nth_param, resolve_key_hash_ops, ret_struct,
};
use super::{HashtableLayout, STATE_OCCUPIED};

pub(crate) fn emit_set_from_list<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    let entry_block = ctx.builder.get_insert_block().unwrap();
    let elem_basic_ty = ir_basic_type(ctx, layout.key_ty)?;

    let list_val = match nth_param(function, llvm_function, 0, "list")? {
        BasicValueEnum::StructValue(v) => v,
        other => {
            return Err(LlvmError::Codegen(format!(
                "Set.from_list expected list struct on `{}`, got `{other:?}`",
                function.symbol,
            )));
        }
    };
    let list_ty = list_value_type(ctx);
    let _ = list_ty;
    let list_ptr = ctx
        .builder
        .build_extract_value(list_val, 0, "list_ptr")
        .map_err(|e| {
            codegen_err(
                format_args!("build_extract_value for `{}`", function.symbol),
                e,
            )
        })?
        .into_pointer_value();
    let list_len = ctx
        .builder
        .build_extract_value(list_val, 1, "list_len")
        .map_err(|e| {
            codegen_err(
                format_args!("build_extract_value for `{}`", function.symbol),
                e,
            )
        })?
        .into_int_value();

    // Mint an empty set, then loop and insert each element. We
    // call back into `build_empty_table` + a freshly-built insert
    // helper rather than the user-facing `Set.new` symbol because
    // the declared-function table may not have either yet at the
    // time `from_list` defines its body.
    let init_set = build_empty_table(ctx, function, layout.entry_size)?;
    let set_alloca = ctx
        .builder
        .build_alloca(hashtable_value_type(ctx), "set_acc")
        .map_err(|e| codegen_err(format_args!("build_alloca for `{}`", function.symbol), e))?;
    ctx.builder
        .build_store(set_alloca, init_set)
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;

    let loop_bb = ctx.context.append_basic_block(llvm_function, "loop");
    let body_bb = ctx.context.append_basic_block(llvm_function, "body");
    let done_bb = ctx.context.append_basic_block(llvm_function, "done");
    ctx.builder
        .build_unconditional_branch(loop_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(loop_bb);
    let i_phi = ctx
        .builder
        .build_phi(i64_ty, "i")
        .map_err(|e| codegen_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    i_phi.add_incoming(&[(&i64_ty.const_zero(), entry_block)]);
    let i_val = i_phi.as_basic_value().into_int_value();
    let done = ctx
        .builder
        .build_int_compare(IntPredicate::UGE, i_val, list_len, "done")
        .map_err(|e| {
            codegen_err(
                format_args!("build_int_compare for `{}`", function.symbol),
                e,
            )
        })?;
    ctx.builder
        .build_conditional_branch(done, done_bb, body_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(body_bb);
    let byte_offset = ctx
        .builder
        .build_int_mul(
            i_val,
            i64_ty.const_int(layout.entry_size, false),
            "byte_off",
        )
        .map_err(|e| codegen_err(format_args!("build_int_mul for `{}`", function.symbol), e))?;
    let elem_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, list_ptr, &[byte_offset], "elem_ptr")
            .map_err(|e| codegen_err(format_args!("build_gep for `{}`", function.symbol), e))?
    };
    let elem_val = ctx
        .builder
        .build_load(elem_basic_ty, elem_ptr, "elem_val")
        .map_err(|e| codegen_err(format_args!("build_load for `{}`", function.symbol), e))?;
    let current = ctx
        .builder
        .build_load(hashtable_value_type(ctx), set_alloca, "cur_set")
        .map_err(|e| codegen_err(format_args!("build_load for `{}`", function.symbol), e))?;
    let updated = call_set_insert_inline(ctx, function, llvm_function, layout, current, elem_val)?;
    ctx.builder
        .build_store(set_alloca, updated)
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    let next_i = ctx
        .builder
        .build_int_add(i_val, i64_ty.const_int(1, false), "next_i")
        .map_err(|e| codegen_err(format_args!("build_int_add for `{}`", function.symbol), e))?;
    let body_tail = ctx.builder.get_insert_block().unwrap();
    i_phi.add_incoming(&[(&next_i, body_tail)]);
    ctx.builder
        .build_unconditional_branch(loop_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(done_bb);
    let final_set = ctx
        .builder
        .build_load(hashtable_value_type(ctx), set_alloca, "final_set")
        .map_err(|e| codegen_err(format_args!("build_load for `{}`", function.symbol), e))?
        .into_struct_value();
    ret_struct(ctx, function, final_set)
}

/// Inline the `Set.insert` body at a call site. v1 emitted a
/// sibling function and called it; this avoids that round-trip by
/// inlining — the per-method declared-function index isn't
/// populated for the freshly-monomorphized `Set.insert` at the
/// point where `from_list`'s body is being emitted.
fn call_set_insert_inline<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    layout: &HashtableLayout<'_>,
    current: BasicValueEnum<'ctx>,
    item: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let i64_ty = ctx.context.i64_type();
    // The receiver is a `current` struct already in hand (not
    // `self_val` from a parameter), so we don't use
    // [`extract_table_fields`] here — the manual 4-extract pattern
    // is the natural fit.
    let current_struct = current.into_struct_value();
    let table = TableSnapshot {
        entries_ptr: extract_pointer(ctx, function, current_struct, 0, "entries")?,
        states_ptr: extract_pointer(ctx, function, current_struct, 1, "states")?,
        length: extract_int(ctx, function, current_struct, 2, "len")?,
        capacity: extract_int(ctx, function, current_struct, 3, "cap")?,
    };
    let key_ops = resolve_key_hash_ops(ctx, function, layout.key_ty)?;

    let post = emit_resize_if_needed(ctx, function, llvm_function, layout, &table, &key_ops)?;
    let probe = emit_insert_probe(ctx, function, llvm_function, layout, &post, item, &key_ops)?;
    // After `emit_insert_probe` returns, the builder is parked on
    // the (already-terminated) `advance` block — appending any
    // instruction here would land it after a terminator, which is
    // malformed IR. Build the merge block fresh and stitch the
    // two outcome paths together with a PHI instead of an alloca.
    let merge_bb = ctx
        .context
        .append_basic_block(llvm_function, "insert_merge");

    ctx.builder.position_at_end(probe.update_bb);
    let dup_result = build_table_struct(
        ctx,
        function,
        post.entries_ptr,
        post.states_ptr,
        post.length,
        post.capacity,
    )?;
    let update_tail = ctx.builder.get_insert_block().ok_or_else(|| {
        LlvmError::Codegen(format!(
            "call_set_insert_inline lost update insertion block on `{}`",
            function.symbol,
        ))
    })?;
    ctx.builder
        .build_unconditional_branch(merge_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(probe.insert_bb);
    let ins_ptr = entry_pointer(
        ctx,
        function,
        post.entries_ptr,
        probe.pidx,
        layout.entry_size,
    )?;
    ctx.builder
        .build_store(ins_ptr, item)
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    ctx.builder
        .build_store(probe.s_ptr, i8_ty.const_int(STATE_OCCUPIED, false))
        .map_err(|e| codegen_err(format_args!("build_store for `{}`", function.symbol), e))?;
    let new_len = ctx
        .builder
        .build_int_add(post.length, i64_ty.const_int(1, false), "new_len")
        .map_err(|e| codegen_err(format_args!("build_int_add for `{}`", function.symbol), e))?;
    let inserted_result = build_table_struct(
        ctx,
        function,
        post.entries_ptr,
        post.states_ptr,
        new_len,
        post.capacity,
    )?;
    let insert_tail = ctx.builder.get_insert_block().ok_or_else(|| {
        LlvmError::Codegen(format!(
            "call_set_insert_inline lost insert insertion block on `{}`",
            function.symbol,
        ))
    })?;
    ctx.builder
        .build_unconditional_branch(merge_bb)
        .map_err(|e| codegen_err(format_args!("build_branch for `{}`", function.symbol), e))?;

    ctx.builder.position_at_end(merge_bb);
    let phi = ctx
        .builder
        .build_phi(hashtable_value_type(ctx), "set_insert_val")
        .map_err(|e| codegen_err(format_args!("build_phi for `{}`", function.symbol), e))?;
    phi.add_incoming(&[(&dup_result, update_tail), (&inserted_result, insert_tail)]);
    Ok(phi.as_basic_value())
}
