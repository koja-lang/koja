//! `Ref<M, R>` and `ReplyTo<R>` `@intrinsic` emitters. Single call
//! site for the per-method `expo_rt_*` declares minted in
//! [`crate::runtime`]; the matching runtime symbols live in
//! `expo-runtime/src/scheduler.rs`.
//!
//! Per-method dispatch:
//!
//! - [`emit_ref`] dispatches each [`RefMethod`] to its emitter:
//!   - `SelfRef` → `expo_rt_self()` wrapped in the `Ref<M, R>`
//!     struct shape.
//!   - `Cast` → serialize `msg`, call `expo_rt_send(pid, blob,
//!     size)`. Today's emit serializes the raw message; the full
//!     `Pair<M, Option<ReplyTo<R>>>` envelope wrapping (matching
//!     the receive-side Business arm protocol) is a follow-up
//!     once the Pair monomorphization is auto-driven from `cast`
//!     call sites.
//!   - `Signal` → `expo_rt_send_lifecycle(pid, variant)` reading
//!     the `Lifecycle` enum tag byte.
//!   - `Kill` → `expo_rt_kill(pid)`.
//!   - `AliveQ` → `expo_rt_is_process_alive(pid) != 0` truncated
//!     back down to `i1`.
//!   - `SendAfter` → serialize `msg`, call
//!     `expo_rt_send_after(pid, blob, size, delay_ms)`.
//!   - `Call` → still a [`LlvmError::Codegen`] today; needs the
//!     Pair envelope + a paired `expo_rt_receive_timeout` reply
//!     loop returning `Result<R, CallError>`.
//! - [`emit_reply_to`] dispatches the single [`ReplyToMethod::Send`]
//!   to a serializer + `expo_rt_send`. Same Pair-wrapping caveat
//!   as `Cast`.

use expo_alpha_ir::{IRFunction, IRType, RefMethod, ReplyToMethod};
use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue};

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::emit::process::serialize_to_stack;
use crate::error::LlvmError;
use crate::runtime::{
    declare_rt_is_process_alive_extern, declare_rt_kill_extern, declare_rt_self_extern,
    declare_rt_send_after_extern, declare_rt_send_extern, declare_rt_send_lifecycle_extern,
};
use crate::types::ir_basic_type;

pub(super) fn emit_ref<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: RefMethod,
) -> Result<(), LlvmError> {
    match method {
        RefMethod::AliveQ => emit_alive(ctx, function, llvm_function),
        RefMethod::Call => Err(LlvmError::Codegen(format!(
            "`Ref.call` (function `{}`) is not yet wired — needs the Pair<M, Option<ReplyTo<R>>> \
             envelope + paired expo_rt_receive_timeout reply loop returning Result<R, CallError>",
            function.symbol,
        ))),
        RefMethod::Cast => emit_cast(ctx, function, llvm_function),
        RefMethod::Kill => emit_kill(ctx, function, llvm_function),
        RefMethod::SelfRef => emit_self_ref(ctx, function, llvm_function),
        RefMethod::SendAfter => emit_send_after(ctx, function, llvm_function),
        RefMethod::Signal => emit_signal(ctx, function, llvm_function),
    }
}

pub(super) fn emit_reply_to<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: ReplyToMethod,
) -> Result<(), LlvmError> {
    match method {
        ReplyToMethod::Send => emit_reply_send(ctx, function, llvm_function),
    }
}

// ----- Ref method emitters -------------------------------------------------

/// `Ref.self_ref() -> Ref<M, R>` — call `expo_rt_self()` and wrap
/// the returned pid in the `Ref` struct value the function's
/// return type already specifies.
fn emit_self_ref<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry_bb = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry_bb);

    let self_fn = declare_rt_self_extern(ctx);
    let pid = ctx
        .builder
        .build_call(self_fn, &[], "current_pid")
        .map_err(|e| inkwell_err("build_call expo_rt_self", e))?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| LlvmError::Codegen("expo_rt_self did not produce a value".to_string()))?
        .into_int_value();

    let ref_struct = match &function.return_type {
        IRType::Struct(symbol) => ctx.layouts.struct_type(symbol.mangled()),
        other => {
            return Err(LlvmError::Codegen(format!(
                "alpha LLVM emit: `Ref.self_ref` returns `{other:?}` (expected Struct) — \
                 IR seal invariant violation",
            )));
        }
    };
    let mut ref_value = ref_struct.get_undef();
    ref_value = ctx
        .builder
        .build_insert_value(ref_value, pid, 0, "ref_pid")
        .map_err(|e| inkwell_err("build_insert_value ref_pid", e))?
        .into_struct_value();
    ctx.builder
        .build_return(Some(&ref_value))
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return self_ref", e))
}

/// `Ref.cast(self, msg: M)` — serialize `msg` into a stack alloca,
/// pull the pid out of `self`, and call
/// `expo_rt_send(pid, blob, size)`.
fn emit_cast<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry_bb = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry_bb);

    let pid = pid_from_self(ctx, llvm_function, function)?;
    let (msg_value, msg_ir_type) = nth_param(function, llvm_function, 1)?;
    let msg_llvm = ir_basic_type(ctx, msg_ir_type)?;
    let (msg_ptr, msg_len) = serialize_to_stack(ctx, "cast_msg", msg_llvm, msg_value)?;

    let send_fn = declare_rt_send_extern(ctx);
    ctx.builder
        .build_call(send_fn, &[pid.into(), msg_ptr.into(), msg_len.into()], "")
        .map_err(|e| inkwell_err("build_call expo_rt_send (cast)", e))?;
    ctx.builder
        .build_return(None)
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return cast", e))
}

/// `Ref.send_after(self, msg: M, delay_ms: Int)` — serialize `msg`
/// and route through `expo_rt_send_after`. Symmetric with
/// [`emit_cast`], plus the trailing delay parameter.
fn emit_send_after<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry_bb = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry_bb);

    let pid = pid_from_self(ctx, llvm_function, function)?;
    let (msg_value, msg_ir_type) = nth_param(function, llvm_function, 1)?;
    let (delay_value, _) = nth_param(function, llvm_function, 2)?;
    let msg_llvm = ir_basic_type(ctx, msg_ir_type)?;
    let (msg_ptr, msg_len) = serialize_to_stack(ctx, "send_after_msg", msg_llvm, msg_value)?;

    let delay = delay_value.into_int_value();
    let send_after_fn = declare_rt_send_after_extern(ctx);
    ctx.builder
        .build_call(
            send_after_fn,
            &[pid.into(), msg_ptr.into(), msg_len.into(), delay.into()],
            "",
        )
        .map_err(|e| inkwell_err("build_call expo_rt_send_after", e))?;
    ctx.builder
        .build_return(None)
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return send_after", e))
}

/// `Ref.signal(self, event: Lifecycle)` — pull the lifecycle
/// variant byte (offset 0 of the enum's outer struct) and call
/// `expo_rt_send_lifecycle(pid, variant)`. The runtime maps
/// variant indices `0=Shutdown, 1=Interrupt, 2=Reload`, matching
/// the AST declaration order in `Global.process`.
fn emit_signal<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry_bb = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry_bb);

    let pid = pid_from_self(ctx, llvm_function, function)?;
    let (event_value, event_ir_type) = nth_param(function, llvm_function, 1)?;
    let event_llvm = ir_basic_type(ctx, event_ir_type)?;
    let event_alloca = ctx.build_entry_alloca(event_llvm, "event_buf");
    ctx.builder
        .build_store(event_alloca, event_value)
        .map_err(|e| inkwell_err("build_store signal event", e))?;
    let i8_ty = ctx.context.i8_type();
    let variant_byte = ctx
        .builder
        .build_load(i8_ty, event_alloca, "variant_byte")
        .map_err(|e| inkwell_err("build_load variant_byte", e))?
        .into_int_value();
    let variant_i64 = ctx
        .builder
        .build_int_z_extend(variant_byte, ctx.context.i64_type(), "variant_i64")
        .map_err(|e| inkwell_err("build_int_z_extend variant", e))?;

    let signal_fn = declare_rt_send_lifecycle_extern(ctx);
    ctx.builder
        .build_call(signal_fn, &[pid.into(), variant_i64.into()], "")
        .map_err(|e| inkwell_err("build_call expo_rt_send_lifecycle", e))?;
    ctx.builder
        .build_return(None)
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return signal", e))
}

/// `Ref.kill(self)` — drop the target process via `expo_rt_kill`.
fn emit_kill<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry_bb = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry_bb);

    let pid = pid_from_self(ctx, llvm_function, function)?;
    let kill_fn = declare_rt_kill_extern(ctx);
    ctx.builder
        .build_call(kill_fn, &[pid.into()], "")
        .map_err(|e| inkwell_err("build_call expo_rt_kill", e))?;
    ctx.builder
        .build_return(None)
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return kill", e))
}

/// `Ref.alive?(self) -> Bool` — compare
/// `expo_rt_is_process_alive(pid)` against zero and return the
/// `i1` result.
fn emit_alive<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry_bb = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry_bb);

    let pid = pid_from_self(ctx, llvm_function, function)?;
    let alive_fn = declare_rt_is_process_alive_extern(ctx);
    let alive_i64 = ctx
        .builder
        .build_call(alive_fn, &[pid.into()], "alive_i64")
        .map_err(|e| inkwell_err("build_call expo_rt_is_process_alive", e))?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| {
            LlvmError::Codegen("expo_rt_is_process_alive did not produce a value".to_string())
        })?
        .into_int_value();
    let zero = ctx.context.i64_type().const_int(0, false);
    let alive_bit = ctx
        .builder
        .build_int_compare(IntPredicate::NE, alive_i64, zero, "is_alive")
        .map_err(|e| inkwell_err("build_int_compare is_alive", e))?;
    ctx.builder
        .build_return(Some(&alive_bit))
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return alive?", e))
}

// ----- ReplyTo method emitters --------------------------------------------

/// `ReplyTo.send(self, reply: R)` — serialize `reply` and route
/// through `expo_rt_send` to the originating caller's pid.
fn emit_reply_send<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let entry_bb = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry_bb);

    let pid = pid_from_self(ctx, llvm_function, function)?;
    let (reply_value, reply_ir_type) = nth_param(function, llvm_function, 1)?;
    let reply_llvm = ir_basic_type(ctx, reply_ir_type)?;
    let (reply_ptr, reply_len) = serialize_to_stack(ctx, "reply_msg", reply_llvm, reply_value)?;

    let send_fn = declare_rt_send_extern(ctx);
    ctx.builder
        .build_call(
            send_fn,
            &[pid.into(), reply_ptr.into(), reply_len.into()],
            "",
        )
        .map_err(|e| inkwell_err("build_call expo_rt_send (reply)", e))?;
    ctx.builder
        .build_return(None)
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return reply send", e))
}

// ----- shared helpers -----------------------------------------------------

/// Pull the i64 pid out of the `self` parameter (always param #0
/// for these methods). `Ref<M, R>` and `ReplyTo<R>` both layout as
/// `{ i64 id }`; we read field 0 directly.
fn pid_from_self<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    function: &IRFunction,
) -> Result<IntValue<'ctx>, LlvmError> {
    let self_value = llvm_function.get_nth_param(0).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "alpha LLVM emit: `{}` missing self parameter",
            function.symbol,
        ))
    })?;
    let self_struct = self_value.into_struct_value();
    ctx.builder
        .build_extract_value(self_struct, 0, "pid")
        .map(|v| v.into_int_value())
        .map_err(|e| inkwell_err("build_extract_value pid", e))
}

/// Read the LLVM value + IR type for the `index`-th parameter,
/// surfacing both for downstream emission. Misses are an upstream
/// IR seal / lower bug.
fn nth_param<'ctx, 'fn_>(
    function: &'fn_ IRFunction,
    llvm_function: FunctionValue<'ctx>,
    index: u32,
) -> Result<(BasicValueEnum<'ctx>, &'fn_ IRType), LlvmError> {
    let value = llvm_function.get_nth_param(index).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "alpha LLVM emit: `{}` missing param #{index}",
            function.symbol,
        ))
    })?;
    let ir_type = function
        .params
        .get(index as usize)
        .map(|p| &p.ty)
        .ok_or_else(|| {
            LlvmError::Codegen(format!(
                "alpha LLVM emit: `{}` IR has no param #{index}",
                function.symbol,
            ))
        })?;
    Ok((value, ir_type))
}
