//! LLVM emit for `IRInstruction::Spawn` / `IRInstruction::Receive`
//! and the [`FunctionKind::SpawnWrapper`] / [`FunctionKind::ProcessEntryWrapper`]
//! bodies. The mailbox surface lives in `koja-runtime-posix/src/scheduler.rs`;
//! this module is the sole call site for the `koja_rt_*` declares
//! minted in [`crate::runtime`].
//!
//! The three pieces snap together as follows:
//!
//! - The lowerer mints a `FunctionKind::SpawnWrapper { state }`
//!   shim per state cell (deduped), whose IR body is a single call
//!   into the IR-synthesized `<state>.__spawn_body` that carries the
//!   real semantics — `start`, the `Result` match, the `run` chain —
//!   under normal ownership lowering. The shim declaration's IR
//!   signature is ignored at LLVM declare time —
//!   [`crate::function::function_signature`] hard-codes a
//!   `void wrapper(i8*)` shape so the symbol is callable through
//!   `koja_rt_spawn`'s `ProcessFn` typedef.
//! - [`emit_spawn_wrapper_body`] / [`emit_process_entry_wrapper_body`]
//!   fill the shim body: load the typed config out of the
//!   runtime-provided pointer and call the process body the IR
//!   `Call` names (plus, for the entry shape, store the returned
//!   exit code into `__koja_exit_code`). The backend synthesizes
//!   nothing beyond this ABI adaptation.
//! - [`emit_spawn`] emits the host-side `IRInstruction::Spawn`:
//!   serializes the config blob into a stack alloca, calls
//!   `koja_rt_spawn(wrapper_ptr, blob_ptr, blob_size)` to mint a
//!   pid, and wraps the pid in a `Ref<M, R>` struct value at
//!   `dest`.
//! - [`emit_receive`] emits the host-side `IRInstruction::Receive`:
//!   calls `koja_rt_receive` (or `koja_rt_receive_timeout` when
//!   `after` is present), inspects the envelope's tag byte,
//!   deserializes the payload into the arm's payload local, and
//!   branches into the matching arm body block. The host block
//!   ends with the dispatch — its IR-level
//!   [`koja_ir::IRTerminator::Unreachable`] terminator is
//!   then a no-op (handled by the already-terminated guard in
//!   [`super::emit_block`]).

use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use koja_ir::{
    IRFunction, IRInstruction, IRSymbol, IRType, ReceiveAfter, ReceiveArm, ReceiveTag, ValueId,
};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::process::payload_drop_glue;
use crate::main_wrapper::EXIT_CODE_SYMBOL;
use crate::runtime::{
    declare_rt_receive_extern, declare_rt_receive_timeout_extern, declare_rt_set_priority_extern,
    declare_rt_spawn_extern, declare_rt_yield_check_extern,
};
use crate::types::{ir_basic_type, value_basic_type};

use super::{ValueMap, lookup};

// ----- wrapper shims --------------------------------------------------------

/// Synthesize the body of a [`koja_ir::FunctionKind::SpawnWrapper`]
/// function. The LLVM declaration is the scheduler's `void(i8*)`
/// `ProcessFn` shape; everything semantic (`start`, the `Result`
/// match, the `run` chain, ownership) lives in the IR-synthesized
/// `<state>.__spawn_body` named by the wrapper's IR `Call`. This
/// emitter only adapts the ABI:
///
/// ```text
/// entry:
///   %config = load <Config>, i8* %0
///   call void @<state>.__spawn_body(<Config> %config)
///   ret void
/// ```
pub(crate) fn emit_spawn_wrapper_body<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    emit_wrapper_shim(ctx, function, llvm_function)?;
    ctx.builder.build_return(None).or_ice().map(|_| ())
}

/// Synthesize the body of a [`koja_ir::FunctionKind::ProcessEntryWrapper`]
/// function. The same ABI adapter as [`emit_spawn_wrapper_body`],
/// plus an exit-code hand-off: `<state>.__entry_body` returns the
/// `i64` exit code (already routed through `Global.StopReason.code`
/// in IR), which the shim truncates and stores into the
/// `__koja_exit_code` global the synthesized `main` trampoline
/// returns after `koja_rt_main_done()` joins the scheduler.
pub(crate) fn emit_process_entry_wrapper_body<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let exit_code = emit_wrapper_shim(ctx, function, llvm_function)?.ok_or_else(|| {
        LlvmError::Codegen(format!(
            "LLVM emit: ProcessEntryWrapper `{}` body call returned no exit code",
            function.symbol,
        ))
    })?;
    store_exit_code(ctx, exit_code.into_int_value())?;
    ctx.builder.build_return(None).or_ice().map(|_| ())
}

/// Shared ABI adaptation: open the entry block, load the typed
/// config out of the runtime-provided `i8*`, and call the process
/// body. Returns the call's result (`None` for the spawn body's
/// `Unit`/`void` return).
fn emit_wrapper_shim<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, LlvmError> {
    let body_symbol = wrapper_body_callee(function)?;
    let body_fn = ctx.declared_function(body_symbol).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "LLVM emit: wrapper `{}` process body `{body_symbol}` not declared",
            function.symbol,
        ))
    })?;
    let config_ir_type = function.params.first().map(|p| &p.ty).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "LLVM emit: wrapper `{}` has no config parameter",
            function.symbol,
        ))
    })?;
    // `value_basic_type` so a `Process<(), _, _>` entry's Unit config
    // loads as the inert `i8` placeholder.
    let config_llvm_type = value_basic_type(ctx, config_ir_type)?;

    let entry_bb = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry_bb);
    let raw_ptr = llvm_function
        .get_nth_param(0)
        .ok_or_else(|| {
            LlvmError::Codegen(format!(
                "LLVM emit: wrapper `{}` declaration has no param #0",
                function.symbol,
            ))
        })?
        .into_pointer_value();
    let typed_config = ctx
        .builder
        .build_load(config_llvm_type, raw_ptr, "loaded_config")
        .or_ice()?;
    let body_call = ctx
        .builder
        .build_call(body_fn, &[typed_config.into()], "")
        .or_ice()?;
    Ok(body_call.try_as_basic_value().basic())
}

/// The process-body symbol named by the wrapper shim's IR `Call` —
/// the single source of truth linking shim to body (no name
/// re-derivation in the backend).
fn wrapper_body_callee(function: &IRFunction) -> Result<&IRSymbol, LlvmError> {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            IRInstruction::Call { callee, .. } => Some(callee),
            _ => None,
        })
        .ok_or_else(|| {
            LlvmError::Codegen(format!(
                "LLVM emit: wrapper `{}` IR body carries no process-body call — lower \
                 invariant violation",
                function.symbol,
            ))
        })
}

/// Truncate the process body's `i64` exit code and store it into the
/// `__koja_exit_code` global the synthesized `main` trampoline
/// returns.
fn store_exit_code<'ctx>(
    ctx: &EmitContext<'ctx>,
    code_i64: IntValue<'ctx>,
) -> Result<(), LlvmError> {
    let code_i32 = ctx
        .builder
        .build_int_truncate(code_i64, ctx.context.i32_type(), "exit_code_i32")
        .or_ice()?;
    let exit_global = ctx.module.get_global(EXIT_CODE_SYMBOL).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "LLVM emit: `{EXIT_CODE_SYMBOL}` global not declared before wrapper body emit",
        ))
    })?;
    ctx.builder
        .build_store(exit_global.as_pointer_value(), code_i32)
        .or_ice()
        .map(|_| ())
}

// ----- IRInstruction::Spawn ------------------------------------------------

/// Emit a single `IRInstruction::Spawn`. Serializes the config
/// value into a stack alloca, hands the raw pointer + byte size +
/// config drop glue to `koja_rt_spawn` along with the wrapper, then
/// wraps the returned pid in a `Ref<M, R>` struct value bound to
/// `dest`. The runtime owns its config copy and runs the glue at
/// process reclaim, so the spawn site transfers the config's nested
/// heap rather than sharing it.
pub(super) fn emit_spawn<'ctx>(
    ctx: &EmitContext<'ctx>,
    config: ValueId,
    config_type: &IRType,
    dest: ValueId,
    ref_type: &IRSymbol,
    wrapper: &IRSymbol,
    values: &mut ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    let config_llvm_type = value_basic_type(ctx, config_type)?;
    let config_value = lookup(values, config)?;

    let (config_ptr, config_size) =
        serialize_to_stack(ctx, "spawn_config", config_llvm_type, config_value)?;
    let drop_glue = payload_drop_glue(ctx, config_type)?;

    let wrapper_fn = ctx.declared_function(wrapper).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "LLVM emit: spawn target wrapper `{wrapper}` not declared",
        ))
    })?;
    let wrapper_ptr = wrapper_fn.as_global_value().as_pointer_value();

    let spawn_fn = declare_rt_spawn_extern(ctx);
    let pid = ctx
        .call_basic(
            spawn_fn,
            &[
                wrapper_ptr.into(),
                config_ptr.into(),
                config_size.into(),
                drop_glue.into(),
            ],
            "spawn_pid",
        )?
        .into_int_value();

    let ref_struct = ctx.layouts.struct_type(ref_type.mangled());
    let mut ref_value = ref_struct.get_undef();
    ref_value = ctx
        .builder
        .build_insert_value(ref_value, pid, 0, "ref_pid")
        .or_ice()?
        .into_struct_value();

    values.insert(dest, ref_value.into());
    Ok(())
}

// ----- IRInstruction::SetPriority ------------------------------------------

/// Emit `IRInstruction::SetPriority`: forward the `Int64` scheduling
/// weight to `koja_rt_set_priority`, retargeting the current process.
/// A straight pass-through call producing no value.
pub(super) fn emit_set_priority<'ctx>(
    ctx: &EmitContext<'ctx>,
    tag: ValueId,
    values: &ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    let level = lookup(values, tag)?.into_int_value();
    let set_priority_fn = declare_rt_set_priority_extern(ctx);
    ctx.builder
        .build_call(set_priority_fn, &[level.into()], "")
        .or_ice()?;
    Ok(())
}

// ----- IRInstruction::YieldCheck -------------------------------------------

/// Emit `IRInstruction::YieldCheck`: a bare call to `koja_rt_yield_check`.
/// No operands, no value — the runtime decides whether to re-queue.
pub(super) fn emit_yield_check(ctx: &EmitContext<'_>) -> Result<(), LlvmError> {
    let yield_check_fn = declare_rt_yield_check_extern(ctx);
    ctx.builder.build_call(yield_check_fn, &[], "").or_ice()?;
    Ok(())
}

// ----- IRInstruction::Receive ----------------------------------------------

/// Emit a single `IRInstruction::Receive`. Allocates a payload scratch
/// slot sized to the widest arm payload, calls `koja_rt_receive` (or
/// `koja_rt_receive_timeout` when `after` is present) to copy the next
/// message's payload into it (the runtime strips the tag header and
/// frees the transport buffer), then branches into the arm whose tag
/// matches the returned wire tag. The host block ends with the dispatch
/// — its IR `Unreachable` terminator is a no-op once the
/// `super::emit_block` already-terminated guard kicks in.
///
/// `dest` and `result_type` come from the IR for symmetry with
/// other instruction emitters; the host block never reads `dest`
/// because dispatch always exits via an arm, so we don't bind
/// anything in `values` for it. Each arm body branches to the
/// `receive_merge` block declared by the lowerer, which is the SSA
/// site that actually defines the `dest` value.
pub(super) fn emit_receive<'ctx>(
    ctx: &EmitContext<'ctx>,
    after: Option<&ReceiveAfter>,
    arms: &[ReceiveArm],
    _dest: ValueId,
    _result_type: &IRType,
    values: &mut ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    let host_block = ctx.builder.get_insert_block().ok_or_else(|| {
        LlvmError::Codegen("LLVM emit: Receive emitted with no insertion block".to_string())
    })?;
    let host_function = host_block.get_parent().ok_or_else(|| {
        LlvmError::Codegen("LLVM emit: Receive's host block has no parent function".to_string())
    })?;

    let (payload_slot, payload_cap) = build_payload_slot(ctx, arms)?;
    let tag_value = build_receive_call(ctx, after, values, payload_slot, payload_cap)?;
    if let Some(after) = after {
        let continue_bb = timeout_tag_branch(ctx, host_function, tag_value, after)?;
        ctx.builder.position_at_end(continue_bb);
    }
    dispatch_arms(ctx, host_function, payload_slot, tag_value, arms)
}

/// Allocate the scratch slot the runtime copies the delivered payload
/// into. Sized to the widest arm payload and 8-aligned (an `i64` array,
/// since every Koja value type is at most 8-aligned), so one slot
/// serves whichever arm matches. Returns the slot pointer and its byte
/// capacity; the runtime clamps the copy to that capacity.
fn build_payload_slot<'ctx>(
    ctx: &EmitContext<'ctx>,
    arms: &[ReceiveArm],
) -> Result<(PointerValue<'ctx>, IntValue<'ctx>), LlvmError> {
    let mut max_size = 0u64;
    for arm in arms {
        let llvm_type = ir_basic_type(ctx, &arm.payload_type)?;
        max_size = max_size.max(ctx.layouts.target_data.get_abi_size(&llvm_type));
    }
    let words = max_size.max(1).div_ceil(8);
    let slot_ty = ctx.context.i64_type().array_type(words as u32);
    let slot = ctx.build_entry_alloca(slot_ty, "receive_payload");
    let cap = ctx.context.i64_type().const_int(max_size, false);
    Ok((slot, cap))
}

/// Lower `koja_rt_receive(slot, cap)` (no timeout) or
/// `koja_rt_receive_timeout(slot, cap, timeout)` to the actual call and
/// return the `i64` wire tag (`-1` when no message). The timeout path
/// lowers the timeout SSA value through the existing value map.
fn build_receive_call<'ctx>(
    ctx: &EmitContext<'ctx>,
    after: Option<&ReceiveAfter>,
    values: &ValueMap<'ctx>,
    payload_slot: PointerValue<'ctx>,
    payload_cap: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let tag_call = if let Some(after) = after {
        let timeout = lookup(values, after.timeout)?.into_int_value();
        let receive_fn = declare_rt_receive_timeout_extern(ctx);
        ctx.builder
            .build_call(
                receive_fn,
                &[payload_slot.into(), payload_cap.into(), timeout.into()],
                "receive_tag",
            )
            .or_ice()?
    } else {
        let receive_fn = declare_rt_receive_extern(ctx);
        ctx.builder
            .build_call(
                receive_fn,
                &[payload_slot.into(), payload_cap.into()],
                "receive_tag",
            )
            .or_ice()?
    };
    Ok(tag_call
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| LlvmError::Codegen("koja_rt_receive did not return a value".to_string()))?
        .into_int_value())
}

/// On the timeout path, branch to the `after` body when the receive
/// returned `-1` (no message arrived within the deadline). Returns the
/// block dispatch continues from once a message was delivered. Wires
/// the `after` arm to its lowered body block via the active block map.
fn timeout_tag_branch<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: FunctionValue<'ctx>,
    tag_value: IntValue<'ctx>,
    after: &ReceiveAfter,
) -> Result<BasicBlock<'ctx>, LlvmError> {
    let none_tag = ctx.context.i64_type().const_int(-1i64 as u64, true);
    let is_none = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, tag_value, none_tag, "receive_is_none")
        .or_ice()?;
    let after_bb = ctx.block_for(after.body);
    let continue_bb = ctx.context.append_basic_block(function, "receive_dispatch");
    ctx.builder
        .build_conditional_branch(is_none, after_bb, continue_bb)
        .or_ice()?;
    Ok(continue_bb)
}

/// Build the per-arm dispatch chain as a sequence of conditional
/// branches keyed on the returned wire tag. Each matching arm gets a
/// dedicated "deserialize then branch to body" block so we can share
/// the payload-load logic without re-emitting it at every comparison
/// site.
///
/// Tags that no arm declares fall through to an `unreachable` —
/// the typecheck seal admits only declared shapes, so a runtime
/// envelope with an unknown tag indicates a runtime/wire-protocol
/// bug.
fn dispatch_arms<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: FunctionValue<'ctx>,
    payload_slot: PointerValue<'ctx>,
    tag_value: IntValue<'ctx>,
    arms: &[ReceiveArm],
) -> Result<(), LlvmError> {
    let i64_ty = ctx.context.i64_type();
    for (index, arm) in arms.iter().enumerate() {
        let wire_byte = arm.tag.wire_byte();
        let is_match = ctx
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag_value,
                i64_ty.const_int(wire_byte as u64, false),
                &format!("is_arm_{index}"),
            )
            .or_ice()?;
        let arm_prelude_bb = ctx
            .context
            .append_basic_block(function, &format!("arm_{index}_prelude"));
        let next_bb = ctx
            .context
            .append_basic_block(function, &format!("arm_{index}_test"));
        ctx.builder
            .build_conditional_branch(is_match, arm_prelude_bb, next_bb)
            .or_ice()?;

        ctx.builder.position_at_end(arm_prelude_bb);
        deserialize_payload_into_local(ctx, payload_slot, arm)?;
        let body_bb = ctx.block_for(arm.body);
        ctx.builder.build_unconditional_branch(body_bb).or_ice()?;

        ctx.builder.position_at_end(next_bb);
    }
    ctx.builder.build_unreachable().or_ice().map(|_| ())
}

/// Load the typed payload the runtime copied into `payload_slot` (the
/// tag header is already stripped) and store it into the arm's payload
/// local. `Business` arms load the message `Pair<M, Option<ReplyTo<R>>>`,
/// `Lifecycle` arms the signal enum, and `IOReady` arms the bare
/// `IOReady` enum that the `elaborate` body rewraps into the `Pair`.
fn deserialize_payload_into_local<'ctx>(
    ctx: &EmitContext<'ctx>,
    payload_slot: PointerValue<'ctx>,
    arm: &ReceiveArm,
) -> Result<(), LlvmError> {
    let payload_llvm_type = ir_basic_type(ctx, &arm.payload_type)?;
    let label = match arm.tag {
        ReceiveTag::Business => "business_payload",
        ReceiveTag::IOReady => "io_ready_payload",
        ReceiveTag::Lifecycle => "lifecycle_payload",
    };
    let payload = ctx
        .builder
        .build_load(payload_llvm_type, payload_slot, label)
        .or_ice()?;
    let slot = ctx.local_slot(arm.payload_local);
    ctx.builder.build_store(slot, payload).or_ice()?;
    Ok(())
}

// ----- shared serializer ---------------------------------------------------

/// Stack-allocate `value` in the entry block, return the pointer
/// (already `i8*`-shaped — opaque pointers in modern LLVM IR mean
/// no bitcast is required) and the type's ABI byte size as `i64`.
/// Used by the spawn / send paths (in this module and the
/// `Ref` / `ReplyTo` intrinsic emitters in
/// [`crate::intrinsics::process`]) to pass typed values across
/// the runtime ABI boundary.
pub(crate) fn serialize_to_stack<'ctx>(
    ctx: &EmitContext<'ctx>,
    label: &str,
    llvm_type: BasicTypeEnum<'ctx>,
    value: BasicValueEnum<'ctx>,
) -> Result<(PointerValue<'ctx>, IntValue<'ctx>), LlvmError> {
    let alloca = ctx.build_entry_alloca(llvm_type, label);
    ctx.builder.build_store(alloca, value).or_ice()?;
    let abi_size = ctx.layouts.target_data.get_abi_size(&llvm_type);
    let size = ctx.context.i64_type().const_int(abi_size, false);
    Ok((alloca, size))
}
