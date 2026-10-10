//! `TraceRuntime.*` `@intrinsic` emitters, the runtime plumbing under
//! the stdlib `Trace` module. Each is backed by a `koja_rt_trace_*`,
//! `koja_rt_span_*`, or `koja_rt_export_*` extern declared in
//! [`crate::runtime`] and implemented in `koja-runtime-posix`.
//!
//! The span record type is opaque here. It rides the `IRFunction`
//! signature, and a record crosses into the runtime the way a message
//! payload does in [`super::process`]: spilled to a stack slot and
//! handed over as bytes plus length plus drop glue. A record coming
//! back is loaded whole from a stack slot the runtime filled.

use inkwell::IntPredicate;
use inkwell::types::BasicType;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use koja_ir::{IRFunction, IRType, IRVariantPayload, TraceRuntimeMethod};

use super::process::payload_drop_glue;
use crate::ctx::EmitContext;
use crate::emit::enums::build_enum_value;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::util::{nth_int, nth_param, nth_param_type};
use crate::runtime::{
    declare_rt_export_dropped_extern, declare_rt_export_pop_extern, declare_rt_export_push_extern,
    declare_rt_span_close_extern, declare_rt_span_id_extern, declare_rt_span_open_extern,
    declare_rt_span_put_extern, declare_rt_span_take_extern, declare_rt_trace_install_extern,
};
use crate::types::ir_basic_type;

pub(super) fn emit_trace_runtime<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: TraceRuntimeMethod,
) -> Result<(), LlvmError> {
    match method {
        TraceRuntimeMethod::ExportDropped => emit_export_dropped(ctx),
        TraceRuntimeMethod::ExportPop => emit_export_pop(ctx, function, llvm_function),
        TraceRuntimeMethod::ExportPush => emit_export_push(ctx, function, llvm_function),
        TraceRuntimeMethod::Install => emit_install(ctx, function, llvm_function),
        TraceRuntimeMethod::SpanClose => emit_span_close(ctx, function, llvm_function),
        TraceRuntimeMethod::SpanId => emit_span_id(ctx),
        TraceRuntimeMethod::SpanOpen => emit_span_open(ctx, function, llvm_function),
        TraceRuntimeMethod::SpanPut => emit_span_put(ctx, function, llvm_function),
        TraceRuntimeMethod::SpanTake => emit_span_take(ctx, function, llvm_function),
    }
}

/// `TraceRuntime.install(context: Process.Context)`: spill the
/// four-word struct to a stack slot and hand it to
/// `koja_rt_trace_install`.
fn emit_install<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let context_value = nth_param(function, llvm_function, 0, "context");
    let slot = ctx
        .builder
        .build_alloca(context_value.get_type(), "context_slot")
        .or_ice()?;
    ctx.builder.build_store(slot, context_value).or_ice()?;
    let install_fn = declare_rt_trace_install_extern(ctx);
    ctx.builder
        .build_call(install_fn, &[slot.into()], "")
        .or_ice()?;
    ctx.builder.build_return(None).or_ice().map(|_| ())
}

/// `TraceRuntime.span_id() -> Int`: `koja_rt_span_id()`.
fn emit_span_id(ctx: &EmitContext<'_>) -> Result<(), LlvmError> {
    let id_fn = declare_rt_span_id_extern(ctx);
    let id = ctx.call_basic(id_fn, &[], "span_id")?;
    ctx.builder.build_return(Some(&id)).or_ice().map(|_| ())
}

/// `TraceRuntime.span_open(record) -> Int`: spill the record and hand
/// it to `koja_rt_span_open`, returning the handle.
fn emit_span_open<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let record_value = nth_param(function, llvm_function, 0, "record");
    let (slot, size) = spill_record(ctx, record_value, "span_record")?;
    let drop_glue = payload_drop_glue(ctx, nth_param_type(function, 0))?;
    let open_fn = declare_rt_span_open_extern(ctx);
    let handle = ctx.call_basic(
        open_fn,
        &[slot.into(), size.into(), drop_glue.into()],
        "span_handle",
    )?;
    ctx.builder.build_return(Some(&handle)).or_ice().map(|_| ())
}

/// `TraceRuntime.span_take(handle: Int) -> record`: let
/// `koja_rt_span_take` fill a stack slot of the record type and load
/// it whole.
fn emit_span_take<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let handle = nth_int(function, llvm_function, 0, "handle");
    let record_llvm = ir_basic_type(ctx, &function.return_type)?;
    let (slot, cap) = record_out_slot(ctx, record_llvm, "taken_record")?;
    let take_fn = declare_rt_span_take_extern(ctx);
    ctx.builder
        .build_call(take_fn, &[handle.into(), slot.into(), cap.into()], "")
        .or_ice()?;
    let record = ctx
        .builder
        .build_load(record_llvm, slot, "record")
        .or_ice()?;
    ctx.builder.build_return(Some(&record)).or_ice().map(|_| ())
}

/// `TraceRuntime.span_put(handle: Int, record)`: spill the record and
/// hand it to `koja_rt_span_put`.
fn emit_span_put<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let handle = nth_int(function, llvm_function, 0, "handle");
    let record_value = nth_param(function, llvm_function, 1, "record");
    let (slot, size) = spill_record(ctx, record_value, "put_record")?;
    let drop_glue = payload_drop_glue(ctx, nth_param_type(function, 1))?;
    let put_fn = declare_rt_span_put_extern(ctx);
    ctx.builder
        .build_call(
            put_fn,
            &[handle.into(), slot.into(), size.into(), drop_glue.into()],
            "",
        )
        .or_ice()?;
    ctx.builder.build_return(None).or_ice().map(|_| ())
}

/// `TraceRuntime.span_close(handle: Int) -> record`: let
/// `koja_rt_span_close` pop the record into a stack slot and load it
/// whole.
fn emit_span_close<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let handle = nth_int(function, llvm_function, 0, "handle");
    let record_llvm = ir_basic_type(ctx, &function.return_type)?;
    let (slot, cap) = record_out_slot(ctx, record_llvm, "closed_record")?;
    let close_fn = declare_rt_span_close_extern(ctx);
    ctx.builder
        .build_call(close_fn, &[handle.into(), slot.into(), cap.into()], "")
        .or_ice()?;
    let record = ctx
        .builder
        .build_load(record_llvm, slot, "record")
        .or_ice()?;
    ctx.builder.build_return(Some(&record)).or_ice().map(|_| ())
}

/// `TraceRuntime.export_push(record)`: spill the record and hand it to
/// `koja_rt_export_push`.
fn emit_export_push<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let record_value = nth_param(function, llvm_function, 0, "record");
    let (slot, size) = spill_record(ctx, record_value, "export_record")?;
    let drop_glue = payload_drop_glue(ctx, nth_param_type(function, 0))?;
    let push_fn = declare_rt_export_push_extern(ctx);
    ctx.builder
        .build_call(push_fn, &[slot.into(), size.into(), drop_glue.into()], "")
        .or_ice()?;
    ctx.builder.build_return(None).or_ice().map(|_| ())
}

/// `TraceRuntime.export_pop() -> Option<record>`: let
/// `koja_rt_export_pop` fill a stack slot of the record type. A `0`
/// status loads the slot into `Option.Some`, `-1` yields
/// `Option.None`.
fn emit_export_pop<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let pop_fn = declare_rt_export_pop_extern(ctx);
    emit_fill_option(
        ctx,
        function,
        llvm_function,
        "TraceRuntime.export_pop",
        pop_fn,
    )
}

/// Shared body of an intrinsic that returns `Option<record>` from a
/// runtime filler with the signature `i64 fill(i8* out, i64 out_cap)`:
/// let `fill_fn` fill a stack slot of the record type. A `0` status
/// loads the slot into `Option.Some`, `-1` yields `Option.None`.
/// `intrinsic` names the caller in the sealed-IR panics.
pub(super) fn emit_fill_option<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    intrinsic: &str,
    fill_fn: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let IRType::Enum(option_symbol) = &function.return_type else {
        panic!(
            "LLVM emit: `{intrinsic}` returns `{:?}`, expected an `Option` enum (IR seal \
             invariant violation)",
            function.return_type,
        );
    };
    let some_tag = ctx.layouts.enum_variant_tag(option_symbol, "Some");
    let none_tag = ctx.layouts.enum_variant_tag(option_symbol, "None");
    let record_type = match ctx.layouts.enum_variant_payload(option_symbol, some_tag) {
        IRVariantPayload::Tuple(types) => match types.as_slice() {
            [record_type] => record_type.clone(),
            other => panic!(
                "LLVM emit: `{intrinsic}` Some payload is `{other:?}`, expected a single \
                 record (IR seal invariant violation)",
            ),
        },
        other => panic!(
            "LLVM emit: `{intrinsic}` Some payload is `{other:?}`, expected a tuple (IR seal \
             invariant violation)",
        ),
    };
    let record_llvm = ir_basic_type(ctx, &record_type)?;
    let (slot, cap) = record_out_slot(ctx, record_llvm, "filled_record")?;

    let status = ctx
        .call_basic(fill_fn, &[slot.into(), cap.into()], "fill_status")?
        .into_int_value();
    let filled = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            status,
            ctx.context.i64_type().const_zero(),
            "filled",
        )
        .or_ice()?;

    let some_bb = ctx.context.append_basic_block(llvm_function, "fill_some");
    let none_bb = ctx.context.append_basic_block(llvm_function, "fill_none");
    let merge_bb = ctx.context.append_basic_block(llvm_function, "fill_merge");
    ctx.builder
        .build_conditional_branch(filled, some_bb, none_bb)
        .or_ice()?;

    ctx.builder.position_at_end(some_bb);
    let record = ctx
        .builder
        .build_load(record_llvm, slot, "record")
        .or_ice()?;
    let some_value = build_enum_value(ctx, option_symbol, some_tag, &[record])?;
    ctx.builder.build_unconditional_branch(merge_bb).or_ice()?;
    let some_block = ctx
        .builder
        .get_insert_block()
        .expect("emit_fill_option lost the some insertion block before the merge phi");

    ctx.builder.position_at_end(none_bb);
    let none_value = build_enum_value(ctx, option_symbol, none_tag, &[])?;
    ctx.builder.build_unconditional_branch(merge_bb).or_ice()?;
    let none_block = ctx
        .builder
        .get_insert_block()
        .expect("emit_fill_option lost the none insertion block before the merge phi");

    ctx.builder.position_at_end(merge_bb);
    let outer = ctx.enum_outer_type(option_symbol.mangled());
    let phi = ctx.builder.build_phi(outer, "fill_option").or_ice()?;
    phi.add_incoming(&[(&some_value, some_block), (&none_value, none_block)]);
    ctx.builder
        .build_return(Some(&phi.as_basic_value()))
        .or_ice()
        .map(|_| ())
}

/// `TraceRuntime.export_dropped() -> Int`: `koja_rt_export_dropped()`.
fn emit_export_dropped(ctx: &EmitContext<'_>) -> Result<(), LlvmError> {
    let dropped_fn = declare_rt_export_dropped_extern(ctx);
    let dropped = ctx.call_basic(dropped_fn, &[], "export_dropped")?;
    ctx.builder
        .build_return(Some(&dropped))
        .or_ice()
        .map(|_| ())
}

/// Spill a record value to a stack slot for a runtime call that copies
/// it out. Returns the slot and the record's ABI size as an `i64`.
pub(super) fn spill_record<'ctx>(
    ctx: &EmitContext<'ctx>,
    record: BasicValueEnum<'ctx>,
    name: &str,
) -> Result<(PointerValue<'ctx>, IntValue<'ctx>), LlvmError> {
    let record_llvm = record.get_type();
    let slot = ctx.builder.build_alloca(record_llvm, name).or_ice()?;
    ctx.builder.build_store(slot, record).or_ice()?;
    Ok((slot, abi_size(ctx, record_llvm)))
}

/// A stack slot of the record type for a runtime call that fills it.
/// Returns the slot and its capacity as an `i64`.
pub(super) fn record_out_slot<'ctx>(
    ctx: &EmitContext<'ctx>,
    record_llvm: inkwell::types::BasicTypeEnum<'ctx>,
    name: &str,
) -> Result<(PointerValue<'ctx>, IntValue<'ctx>), LlvmError> {
    let slot = ctx.build_entry_alloca(record_llvm, name);
    Ok((slot, abi_size(ctx, record_llvm)))
}

fn abi_size<'ctx>(
    ctx: &EmitContext<'ctx>,
    ty: inkwell::types::BasicTypeEnum<'ctx>,
) -> IntValue<'ctx> {
    let size = ctx
        .layouts
        .target_data
        .get_abi_size(&ty.as_basic_type_enum());
    ctx.context.i64_type().const_int(size, false)
}
