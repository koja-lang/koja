//! Envelope construction for the message-bearing process methods.
//! `Ref.cast`, `Ref.send_after`, and `Ref.call` all hand the runtime
//! a stack `(M, Option<ReplyTo<R>>)` tuple, and every send site
//! pairs its payload with a by-pointer drop shim the runtime calls
//! when it discards an undelivered message.

use inkwell::AddressSpace;
use inkwell::types::{BasicType, BasicTypeEnum, StructType};
use inkwell::values::{ArrayValue, BasicValueEnum, FunctionValue, IntValue, PointerValue};
use koja_ir::IRType;
use koja_ir::mangling::{drop_glue_symbol, envelope_drop_glue_symbol};

use crate::ctx::EmitContext;
use crate::emit::heap_layout::is_heap_leaf;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::element::{ElementOp, apply_in_slot};
use crate::layout::wire_contract::{OPTION_NONE_TAG, OPTION_SOME_TAG};

/// Stack-allocate an `(M, Option<ReplyTo<R>>)` envelope with
/// `msg_value` in the first element and `option_payload` in the
/// second, then return `(envelope_ptr, abi_size)` ready for an
/// `koja_rt_send` / `koja_rt_send_after` call.
pub(super) fn build_tuple_envelope_alloca<'ctx>(
    ctx: &EmitContext<'ctx>,
    label: &str,
    msg_ty: BasicTypeEnum<'ctx>,
    msg_value: BasicValueEnum<'ctx>,
    option_payload: ArrayValue<'ctx>,
) -> Result<(PointerValue<'ctx>, IntValue<'ctx>), LlvmError> {
    let envelope_ty: StructType<'ctx> = ctx
        .context
        .struct_type(&[msg_ty, option_reply_to_payload_ty(ctx)], false);
    let alloca = ctx.build_entry_alloca(envelope_ty, label);
    let undef = envelope_ty.get_undef();
    let with_msg = ctx
        .builder
        .build_insert_value(undef, msg_value, 0, "tuple_msg")
        .or_ice()?
        .into_struct_value();
    let envelope = ctx
        .builder
        .build_insert_value(with_msg, option_payload, 1, "tuple_option")
        .or_ice()?
        .into_struct_value();
    ctx.builder.build_store(alloca, envelope).or_ice()?;
    let abi_size = ctx
        .layouts
        .target_data
        .get_abi_size(&envelope_ty.as_basic_type_enum());
    let size = ctx.context.i64_type().const_int(abi_size, false);
    Ok((alloca, size))
}

/// The second-field bytes for an `Option::None` reply slot
/// (`Ref.cast` / `Ref.send_after`), `[OPTION_NONE_TAG, 0, 0]`.
pub(super) fn option_none_payload<'ctx>(ctx: &EmitContext<'ctx>) -> ArrayValue<'ctx> {
    let i64_ty = ctx.context.i64_type();
    i64_ty.const_array(&[
        i64_ty.const_int(OPTION_NONE_TAG, false),
        i64_ty.const_int(0, false),
        i64_ty.const_int(0, false),
    ])
}

/// The second-field bytes for an `Option::Some(ReplyTo { id:
/// reply_pid, token })` reply slot (`Ref.call`),
/// `[OPTION_SOME_TAG, reply_pid, token]`. On little-endian hosts the tag byte sits in
/// the low byte of the first `i64`, with the trailing 7 padding
/// bytes zeroed, and the reply pid and correlation token occupy the
/// next two. SSA-built because both are SSA results
/// (`koja_rt_self()` / `koja_rt_call_token()`).
pub(super) fn option_some_payload<'ctx>(
    ctx: &EmitContext<'ctx>,
    reply_pid: IntValue<'ctx>,
    token: IntValue<'ctx>,
) -> Result<ArrayValue<'ctx>, LlvmError> {
    let i64_ty = ctx.context.i64_type();
    let undef = i64_ty.array_type(3).get_undef();
    let with_tag = ctx
        .builder
        .build_insert_value(
            undef,
            i64_ty.const_int(OPTION_SOME_TAG, false),
            0,
            "opt_tag",
        )
        .or_ice()?;
    let with_pid = ctx
        .builder
        .build_insert_value(with_tag, reply_pid, 1, "opt_pid")
        .or_ice()?;
    Ok(ctx
        .builder
        .build_insert_value(with_pid, token, 2, "opt_token")
        .or_ice()?
        .into_array_value())
}

/// Synthesized LLVM type for the second element of an
/// `(M, Option<ReplyTo<R>>)` envelope. `R` has no LLVM-side influence.
/// `ReplyTo<R>` always lays out as `{ i64 id, i64 token }`, so
/// `Option<ReplyTo<R>>` is `{ i8 tag, [7 x i8] padding, i64
/// reply_id, i64 token }` = 24 bytes regardless of `R`. We pack it
/// into `[3 x i64]` so the writer side does not need the
/// receive-side's pre-emit Option registry lookup. Binary
/// layout matches the receiver's typed load by construction.
fn option_reply_to_payload_ty<'ctx>(ctx: &EmitContext<'ctx>) -> BasicTypeEnum<'ctx> {
    ctx.context.i64_type().array_type(3).into()
}

/// Build (or look up) the by-pointer payload drop shim for `payload`
/// and return its address as the `void(i8*)*` value the `koja_rt_send`
/// / `koja_rt_send_after` / `koja_rt_reply` / `koja_rt_spawn`
/// `drop_glue` argument expects. Returns a null pointer when the
/// payload owns no nested Koja heap (scalars, no-glue aggregates),
/// in which case the runtime frees only its own buffer on discard.
///
/// The runtime's discard path is type-erased (`fn(*mut u8)` over the
/// payload bytes), an ABI the by-value `drop_T` cannot satisfy. The
/// shim bridges the two. It loads `payload` through the pointer and
/// routes into `drop_T` via [`apply_in_slot`]. Content-addressed by
/// [`envelope_drop_glue_symbol`], so every send site for the same
/// message / reply / config type shares one shim.
pub(crate) fn payload_drop_glue<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: &IRType,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    if !payload_owns_heap(ctx, payload) {
        return Ok(ptr_ty.const_null().into());
    }
    let symbol = envelope_drop_glue_symbol(payload);
    let shim = match ctx.module.get_function(symbol.mangled()) {
        Some(existing) => existing,
        None => build_payload_drop_shim(ctx, payload, symbol.mangled())?,
    };
    Ok(shim.as_global_value().as_pointer_value().into())
}

/// Whether `payload` carries any nested Koja heap to release on
/// discard, that is a heap leaf (`String` / `Binary` / `Bits`) or a
/// composite with declared `drop_T`. Scalars and no-glue aggregates answer
/// `false`, so [`payload_drop_glue`] hands the runtime a null glue.
fn payload_owns_heap(ctx: &EmitContext<'_>, payload: &IRType) -> bool {
    is_heap_leaf(payload) || ctx.declared_function(&drop_glue_symbol(payload)).is_some()
}

/// Synthesize the `void(i8*)` envelope-drop shim body, which loads
/// the payload through its pointer and releases it via
/// [`apply_in_slot`]. Saves and restores the builder position so it
/// can be minted in the middle of a send emitter's body.
fn build_payload_drop_shim<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload: &IRType,
    symbol: &str,
) -> Result<FunctionValue<'ctx>, LlvmError> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = ctx.context.void_type().fn_type(&[ptr_ty.into()], false);
    let shim = ctx.module.add_function(symbol, signature, None);
    let saved = ctx.builder.get_insert_block();
    let entry = ctx.context.append_basic_block(shim, "entry");
    ctx.builder.position_at_end(entry);
    let payload_ptr = shim
        .get_nth_param(0)
        .unwrap_or_else(|| panic!("envelope drop shim `{symbol}` missing payload param"))
        .into_pointer_value();
    apply_in_slot(ctx, ElementOp::Release, payload, payload_ptr)?;
    ctx.builder.build_return(None).or_ice()?;
    if let Some(saved) = saved {
        ctx.builder.position_at_end(saved);
    }
    Ok(shim)
}
