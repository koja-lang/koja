//! LLVM emit for `IRInstruction::Spawn` / `IRInstruction::Receive`
//! and the [`FunctionKind::SpawnWrapper`] / [`FunctionKind::ProcessEntryWrapper`]
//! bodies. The mailbox surface lives in `koja-runtime/src/scheduler.rs`;
//! this module is the sole call site for the `koja_rt_*` declares
//! minted in [`crate::runtime`].
//!
//! The three pieces snap together as follows:
//!
//! - The lowerer mints a `FunctionKind::SpawnWrapper { state }`
//!   thunk per state cell (deduped). That declaration's IR
//!   signature is ignored at LLVM declare time —
//!   [`crate::function::function_signature`] hard-codes a
//!   `void wrapper(i8*)` shape so the symbol is callable through
//!   `koja_rt_spawn`'s `ProcessFn` typedef.
//! - [`emit_spawn_wrapper_body`] supplies the wrapper's body:
//!   reads the typed config out of the runtime-provided pointer,
//!   calls `<state>.start(config)` (a `Result<state, StopReason>`),
//!   and on `Result.Ok` chains into `<state>.run(state)` so the
//!   process keeps draining its mailbox until `run` returns.
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

use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use koja_ir::mangling::mangled_method_name;
use koja_ir::{
    IRFunction, IRSymbol, IRType, IRVariantTag, ReceiveAfter, ReceiveArm, ReceiveTag, ValueId,
};

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;
use crate::main_wrapper::EXIT_CODE_SYMBOL;
use crate::runtime::{
    declare_rt_receive_extern, declare_rt_receive_timeout_extern, declare_rt_spawn_extern,
};
use crate::types::ir_basic_type;

use super::{ValueMap, lookup};

// ----- SpawnWrapper body ---------------------------------------------------

/// Synthesize the body of a `FunctionKind::SpawnWrapper { state }`
/// function. The LLVM declaration has signature `void(i8*)`; this
/// emitter ignores the IR-level placeholder body and fills the
/// LLVM body with the actual scheduler entrypoint:
///
/// ```text
/// entry:
///   %typed_ptr = bitcast i8* %0 to <Config>*
///   %config = load <Config>, <Config>* %typed_ptr
///   %result = call <Result<State, StopReason>> @<State>.start(<Config> %config)
///   %tag = extractvalue <Result> %result, 0
///   %is_ok = icmp eq i8 %tag, 0
///   br i1 %is_ok, label %ok, label %err
/// ok:
///   %state = extractvalue <Result> %result, 1   ; payload is State
///   call void @<State>.run(<State> %state)        ; the run loop
///   ret void
/// err:
///   ret void
/// ```
///
/// The `start` and `run` siblings are looked up by symbol
/// (`<state>.start` / `<state>.run`). The ABI uses unboxed structs
/// throughout — `extractvalue` / `insertvalue` walk the aggregate
/// shape exactly as the lowerer + struct/enum layout pre-emit
/// produced it.
pub(crate) fn emit_spawn_wrapper_body<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    state: &IRType,
) -> Result<(), LlvmError> {
    let ctx_wrapper = WrapperBodyCtx::resolve(ctx, function, llvm_function, state, "SpawnWrapper")?;
    let StartBranch {
        ok_bb,
        err_bb,
        ok_complete,
        ok_payload,
        result_alloca,
        ..
    } = ctx_wrapper.emit_start_dispatch(ctx)?;

    // Ok path: extract state, fire-and-forget run() — its return is
    // discarded because the scheduler manages the spawned process's
    // lifecycle through receive loops and shutdown signals.
    ctx.builder.position_at_end(ok_bb);
    let state_val = load_ok_state(ctx, &ctx_wrapper, ok_complete, ok_payload, result_alloca)?;
    ctx.builder
        .build_call(ctx_wrapper.run_fn, &[state_val.into()], "")
        .map_err(|e| inkwell_err("build_call run", e))?;
    ctx.builder
        .build_return(None)
        .map_err(|e| inkwell_err("build_return wrapper ok", e))?;

    ctx.builder.position_at_end(err_bb);
    ctx.builder
        .build_return(None)
        .map_err(|e| inkwell_err("build_return wrapper err", e))?;

    Ok(())
}

/// Synthesize the body of a [`koja_ir::FunctionKind::ProcessEntryWrapper`]
/// function. Extends [`emit_spawn_wrapper_body`] with an exit-code
/// hand-off so the synthesized `main` trampoline can return a
/// process exit status:
///
/// - On `start` returning `Ok(state)`, the wrapper calls
///   `state.run(state)`, threads the resulting `StopReason` through
///   `Global.StopReason.code()`, truncates the `i64` to `i32`, and
///   stores it into the `__koja_exit_code` global.
/// - On `start` returning `Err(stop_reason)`, the wrapper hands the
///   `StopReason` directly to `Global.StopReason.code()`, truncates,
///   and stores. The wrapper always `ret void`s — the trampoline's
///   `ret i32` reads the global after `koja_rt_main_done()` joins
///   the scheduler.
pub(crate) fn emit_process_entry_wrapper_body<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    state: &IRType,
) -> Result<(), LlvmError> {
    let ctx_wrapper =
        WrapperBodyCtx::resolve(ctx, function, llvm_function, state, "ProcessEntryWrapper")?;
    let code_symbol = "Global.StopReason.code";
    let code_fn = ctx.module.get_function(code_symbol).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "LLVM emit: ProcessEntryWrapper `{}` cannot resolve StopReason method \
             `{code_symbol}`",
            function.symbol,
        ))
    })?;

    let StartBranch {
        ok_bb,
        err_bb,
        ok_complete,
        ok_payload,
        result_alloca,
        result_outer_name,
    } = ctx_wrapper.emit_start_dispatch(ctx)?;

    // Ok path: extract state, call run, then route run's StopReason
    // return through StopReason.code() into __koja_exit_code.
    ctx.builder.position_at_end(ok_bb);
    let state_val = load_ok_state(ctx, &ctx_wrapper, ok_complete, ok_payload, result_alloca)?;
    let run_call = ctx
        .builder
        .build_call(ctx_wrapper.run_fn, &[state_val.into()], "stop_reason")
        .map_err(|e| inkwell_err("build_call run", e))?;
    let stop_reason = run_call.try_as_basic_value().basic().ok_or_else(|| {
        LlvmError::Codegen("run() did not produce a StopReason value".to_string())
    })?;
    store_exit_code(ctx, code_fn, stop_reason)?;
    ctx.builder
        .build_return(None)
        .map_err(|e| inkwell_err("build_return entry wrapper ok", e))?;

    // Err path: extract the StopReason payload from Result.Err, hand
    // it to StopReason.code(), store the exit code.
    ctx.builder.position_at_end(err_bb);
    let stop_reason_val = load_err_stop_reason(
        ctx,
        &ctx_wrapper,
        &result_outer_name,
        result_alloca,
        code_fn,
    )?;
    store_exit_code(ctx, code_fn, stop_reason_val)?;
    ctx.builder
        .build_return(None)
        .map_err(|e| inkwell_err("build_return entry wrapper err", e))?;

    Ok(())
}

/// Shared inputs both wrapper bodies need before they branch. Owns
/// the `start` + `run` function handles and the typed state LLVM
/// type so the per-kind tails can keep their dispatch shape flat.
struct WrapperBodyCtx<'ctx> {
    state_llvm_type: BasicTypeEnum<'ctx>,
    start_fn: FunctionValue<'ctx>,
    run_fn: FunctionValue<'ctx>,
    typed_config: BasicValueEnum<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    function_label: IRSymbol,
}

struct StartBranch<'ctx> {
    ok_bb: BasicBlock<'ctx>,
    err_bb: BasicBlock<'ctx>,
    ok_complete: StructType<'ctx>,
    ok_payload: Option<StructType<'ctx>>,
    result_alloca: PointerValue<'ctx>,
    result_outer_name: String,
}

impl<'ctx> WrapperBodyCtx<'ctx> {
    fn resolve(
        ctx: &EmitContext<'ctx>,
        function: &IRFunction,
        llvm_function: FunctionValue<'ctx>,
        state: &IRType,
        kind_label: &str,
    ) -> Result<Self, LlvmError> {
        let IRType::Struct(state_symbol) = state else {
            return Err(LlvmError::Codegen(format!(
                "LLVM emit: {kind_label} `{}` declared with non-struct state `{state:?}` — \
                 IR seal invariant violation",
                function.symbol,
            )));
        };

        let config_ir_type = function
            .params
            .first()
            .map(|p| p.ty.clone())
            .ok_or_else(|| {
                LlvmError::Codegen(format!(
                    "LLVM emit: {kind_label} `{}` has no config parameter",
                    function.symbol,
                ))
            })?;
        let config_llvm_type = ir_basic_type(ctx, &config_ir_type)?;

        let start_symbol = mangled_method_name(state_symbol, &[], "start", &[]);
        let start_fn = ctx.declared_function(&start_symbol).ok_or_else(|| {
            LlvmError::Codegen(format!(
                "LLVM emit: {kind_label} `{}` cannot resolve start method `{start_symbol}`",
                function.symbol,
            ))
        })?;
        let run_symbol = mangled_method_name(state_symbol, &[], "run", &[]);
        let run_fn = ctx.declared_function(&run_symbol).ok_or_else(|| {
            LlvmError::Codegen(format!(
                "LLVM emit: {kind_label} `{}` cannot resolve run method `{run_symbol}`",
                function.symbol,
            ))
        })?;

        let entry_bb = ctx.context.append_basic_block(llvm_function, "entry");
        ctx.builder.position_at_end(entry_bb);
        let raw_ptr = llvm_function
            .get_nth_param(0)
            .ok_or_else(|| {
                LlvmError::Codegen(format!(
                    "LLVM emit: {kind_label} `{}` declaration has no param #0",
                    function.symbol,
                ))
            })?
            .into_pointer_value();
        let typed_config = ctx
            .builder
            .build_load(config_llvm_type, raw_ptr, "loaded_config")
            .map_err(|e| inkwell_err("build_load loaded_config", e))?;

        let state_llvm_type = ir_basic_type(ctx, state)?;

        Ok(Self {
            state_llvm_type,
            start_fn,
            run_fn,
            typed_config,
            llvm_function,
            function_label: function.symbol.clone(),
        })
    }

    fn emit_start_dispatch(&self, ctx: &EmitContext<'ctx>) -> Result<StartBranch<'ctx>, LlvmError> {
        let start_call = ctx
            .builder
            .build_call(self.start_fn, &[self.typed_config.into()], "start_result")
            .map_err(|e| inkwell_err("build_call start", e))?;
        let result_value = start_call
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| LlvmError::Codegen("start() did not produce a value".to_string()))?;

        let result_outer = result_value.into_struct_value().get_type();
        let result_outer_name = result_outer
            .get_name()
            .and_then(|n| n.to_str().ok())
            .ok_or_else(|| {
                LlvmError::Codegen(format!(
                    "LLVM emit: wrapper `{}` could not resolve start return type's struct \
                     name",
                    self.function_label,
                ))
            })?
            .to_string();
        let (ok_complete, ok_payload) = ctx
            .layouts
            .enum_variant_types(&result_outer_name, IRVariantTag(0));
        let result_alloca = ctx
            .builder
            .build_alloca(result_outer, "result")
            .map_err(|e| inkwell_err("build_alloca result", e))?;
        ctx.builder
            .build_store(result_alloca, result_value)
            .map_err(|e| inkwell_err("build_store result", e))?;

        let ok_bb = ctx
            .context
            .append_basic_block(self.llvm_function, "start_ok");
        let err_bb = ctx
            .context
            .append_basic_block(self.llvm_function, "start_err");

        let i8_ty = ctx.context.i8_type();
        let tag = read_variant_tag(ctx, ok_complete, result_alloca)?;
        let is_ok = ctx
            .builder
            .build_int_compare(IntPredicate::EQ, tag, i8_ty.const_int(0, false), "is_ok")
            .map_err(|e| inkwell_err("build_int_compare is_ok", e))?;
        ctx.builder
            .build_conditional_branch(is_ok, ok_bb, err_bb)
            .map_err(|e| inkwell_err("build_conditional_branch wrapper", e))?;

        Ok(StartBranch {
            ok_bb,
            err_bb,
            ok_complete,
            ok_payload,
            result_alloca,
            result_outer_name,
        })
    }
}

fn load_ok_state<'ctx>(
    ctx: &EmitContext<'ctx>,
    wrapper: &WrapperBodyCtx<'ctx>,
    ok_complete: StructType<'ctx>,
    ok_payload: Option<StructType<'ctx>>,
    result_alloca: PointerValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let ok_payload_struct = ok_payload.ok_or_else(|| {
        LlvmError::Codegen(format!(
            "LLVM emit: wrapper `{}` start return type's `Ok` variant declares no payload \
             — IR seal invariant violation",
            wrapper.function_label,
        ))
    })?;
    let payload_struct_ptr = ctx
        .builder
        .build_struct_gep(ok_complete, result_alloca, 2, "ok_payload_struct")
        .map_err(|e| inkwell_err("build_struct_gep ok_payload_struct", e))?;
    let state_ptr = ctx
        .builder
        .build_struct_gep(ok_payload_struct, payload_struct_ptr, 0, "ok_state_field")
        .map_err(|e| inkwell_err("build_struct_gep ok_state_field", e))?;
    ctx.builder
        .build_load(wrapper.state_llvm_type, state_ptr, "state")
        .map_err(|e| inkwell_err("build_load state", e))
}

fn load_err_stop_reason<'ctx>(
    ctx: &EmitContext<'ctx>,
    wrapper: &WrapperBodyCtx<'ctx>,
    result_outer_name: &str,
    result_alloca: PointerValue<'ctx>,
    code_fn: FunctionValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, LlvmError> {
    let stop_reason_llvm_type = code_fn
        .get_type()
        .get_param_types()
        .into_iter()
        .next()
        .ok_or_else(|| {
            LlvmError::Codegen(format!(
                "LLVM emit: wrapper `{}` StopReason.code has no receiver parameter",
                wrapper.function_label,
            ))
        })?
        .into_struct_type();
    let (err_complete, err_payload) = ctx
        .layouts
        .enum_variant_types(result_outer_name, IRVariantTag(1));
    let err_payload_struct = err_payload.ok_or_else(|| {
        LlvmError::Codegen(format!(
            "LLVM emit: wrapper `{}` start return type's `Err` variant declares no payload \
             — IR seal invariant violation",
            wrapper.function_label,
        ))
    })?;
    let payload_struct_ptr = ctx
        .builder
        .build_struct_gep(err_complete, result_alloca, 2, "err_payload_struct")
        .map_err(|e| inkwell_err("build_struct_gep err_payload_struct", e))?;
    let stop_reason_ptr = ctx
        .builder
        .build_struct_gep(
            err_payload_struct,
            payload_struct_ptr,
            0,
            "err_stop_reason_field",
        )
        .map_err(|e| inkwell_err("build_struct_gep err_stop_reason_field", e))?;
    ctx.builder
        .build_load(stop_reason_llvm_type, stop_reason_ptr, "stop_reason")
        .map_err(|e| inkwell_err("build_load stop_reason", e))
}

fn store_exit_code<'ctx>(
    ctx: &EmitContext<'ctx>,
    code_fn: FunctionValue<'ctx>,
    stop_reason: BasicValueEnum<'ctx>,
) -> Result<(), LlvmError> {
    let code_call = ctx
        .builder
        .build_call(code_fn, &[stop_reason.into()], "exit_code_i64")
        .map_err(|e| inkwell_err("build_call StopReason.code", e))?;
    let code_i64 = code_call
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| LlvmError::Codegen("StopReason.code did not produce a value".to_string()))?
        .into_int_value();
    let code_i32 = ctx
        .builder
        .build_int_truncate(code_i64, ctx.context.i32_type(), "exit_code_i32")
        .map_err(|e| inkwell_err("build_int_truncate exit_code", e))?;
    let exit_global = ctx.module.get_global(EXIT_CODE_SYMBOL).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "LLVM emit: `{EXIT_CODE_SYMBOL}` global not declared before wrapper body emit",
        ))
    })?;
    ctx.builder
        .build_store(exit_global.as_pointer_value(), code_i32)
        .map(|_| ())
        .map_err(|e| inkwell_err("build_store __koja_exit_code", e))
}

/// Read the `i8` variant tag (always at field 0 of every variant's
/// `complete` struct) out of a value spilled to `slot`. The IR-
/// level `EnumTagGet` instruction emits the same shape; we
/// duplicate it inline here because the wrapper doesn't run inside
/// the IR-instruction emit loop.
fn read_variant_tag<'ctx>(
    ctx: &EmitContext<'ctx>,
    variant_complete: StructType<'ctx>,
    slot: PointerValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let tag_ptr = ctx
        .builder
        .build_struct_gep(variant_complete, slot, 0, "tag_ptr")
        .map_err(|e| inkwell_err("build_struct_gep tag", e))?;
    ctx.builder
        .build_load(ctx.context.i8_type(), tag_ptr, "tag")
        .map(|v| v.into_int_value())
        .map_err(|e| inkwell_err("build_load tag", e))
}

// ----- IRInstruction::Spawn ------------------------------------------------

/// Emit a single `IRInstruction::Spawn`. Serializes the config
/// value into a stack alloca, hands the raw pointer + byte size to
/// `koja_rt_spawn` along with the wrapper, then wraps the returned
/// pid in a `Ref<M, R>` struct value bound to `dest`.
pub(super) fn emit_spawn<'ctx>(
    ctx: &EmitContext<'ctx>,
    config: ValueId,
    config_type: &IRType,
    dest: ValueId,
    ref_type: &IRSymbol,
    wrapper: &IRSymbol,
    values: &mut ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    let config_llvm_type = ir_basic_type(ctx, config_type)?;
    let config_value = lookup(values, config)?;

    let (config_ptr, config_size) =
        serialize_to_stack(ctx, "spawn_config", config_llvm_type, config_value)?;

    let wrapper_fn = ctx.declared_function(wrapper).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "LLVM emit: spawn target wrapper `{wrapper}` not declared",
        ))
    })?;
    let wrapper_ptr = wrapper_fn.as_global_value().as_pointer_value();

    let spawn_fn = declare_rt_spawn_extern(ctx);
    let pid_call = ctx
        .builder
        .build_call(
            spawn_fn,
            &[wrapper_ptr.into(), config_ptr.into(), config_size.into()],
            "spawn_pid",
        )
        .map_err(|e| inkwell_err("build_call koja_rt_spawn", e))?;
    let pid = pid_call
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| LlvmError::Codegen("koja_rt_spawn did not produce a value".to_string()))?
        .into_int_value();

    let ref_struct = ctx.layouts.struct_type(ref_type.mangled());
    let mut ref_value = ref_struct.get_undef();
    ref_value = ctx
        .builder
        .build_insert_value(ref_value, pid, 0, "ref_pid")
        .map_err(|e| inkwell_err("build_insert_value ref_pid", e))?
        .into_struct_value();

    values.insert(dest, ref_value.into());
    Ok(())
}

// ----- IRInstruction::Receive ----------------------------------------------

/// Byte offset of the payload inside an envelope buffer. The
/// runtime allocates `8` bytes for the tag (with padding) before
/// the payload; this constant is the single source of truth on the
/// LLVM side. Mirrors `TAG_HEADER_SIZE` in
/// `koja-runtime/src/scheduler.rs`. Per-arm tag bytes
/// (`Lifecycle = 1`, `Business = 0`) ride through
/// [`ReceiveTag::wire_byte`] so the LLVM and runtime sides agree
/// at a single source of truth.
const ENVELOPE_PAYLOAD_OFFSET: u64 = 8;

/// Emit a single `IRInstruction::Receive`. Calls `koja_rt_receive`
/// (or `koja_rt_receive_timeout` when `after` is present), reads
/// the envelope tag, deserializes the matching arm's payload into
/// its declared local slot, and branches into the arm body block.
/// The host block ends with the dispatch — its IR `Unreachable`
/// terminator is a no-op once the `super::emit_block` already-
/// terminated guard kicks in.
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

    let envelope_ptr = build_receive_call(ctx, after, values)?;
    let after_branch = match after {
        Some(after) => Some(timeout_null_branch(
            ctx,
            host_function,
            envelope_ptr,
            after,
        )?),
        None => None,
    };

    if let Some((continue_bb, _)) = after_branch {
        ctx.builder.position_at_end(continue_bb);
    }
    let tag_value = read_envelope_tag(ctx, envelope_ptr)?;
    dispatch_arms(ctx, host_function, envelope_ptr, tag_value, arms)
}

/// Lower `koja_rt_receive` (no timeout) or
/// `koja_rt_receive_timeout(timeout)` to the actual call and
/// return the `i8*` envelope pointer. The timeout path lowers the
/// timeout SSA value through the existing value map.
fn build_receive_call<'ctx>(
    ctx: &EmitContext<'ctx>,
    after: Option<&ReceiveAfter>,
    values: &ValueMap<'ctx>,
) -> Result<PointerValue<'ctx>, LlvmError> {
    let envelope_call = if let Some(after) = after {
        let timeout = lookup(values, after.timeout)?.into_int_value();
        let receive_fn = declare_rt_receive_timeout_extern(ctx);
        ctx.builder
            .build_call(receive_fn, &[timeout.into()], "receive_envelope")
            .map_err(|e| inkwell_err("build_call koja_rt_receive_timeout", e))?
    } else {
        let receive_fn = declare_rt_receive_extern(ctx);
        ctx.builder
            .build_call(receive_fn, &[], "receive_envelope")
            .map_err(|e| inkwell_err("build_call koja_rt_receive", e))?
    };
    Ok(envelope_call
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| LlvmError::Codegen("koja_rt_receive did not return a value".to_string()))?
        .into_pointer_value())
}

/// On the timeout path, branch to the `after` body when
/// `koja_rt_receive_timeout` returns null (no message arrived
/// within the deadline). Returns the basic block the dispatch
/// should continue from when the envelope is non-null. Wires the
/// `after` arm to its lowered body block via the active block map.
fn timeout_null_branch<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: FunctionValue<'ctx>,
    envelope_ptr: PointerValue<'ctx>,
    after: &ReceiveAfter,
) -> Result<(BasicBlock<'ctx>, ()), LlvmError> {
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let null_ptr = ptr_ty.const_null();
    let is_null = ctx
        .builder
        .build_int_compare(IntPredicate::EQ, envelope_ptr, null_ptr, "envelope_is_null")
        .map_err(|e| inkwell_err("build_int_compare envelope_is_null", e))?;
    let after_bb = ctx.block_for(after.body);
    let continue_bb = ctx.context.append_basic_block(function, "receive_dispatch");
    ctx.builder
        .build_conditional_branch(is_null, after_bb, continue_bb)
        .map_err(|e| inkwell_err("build_conditional_branch envelope_is_null", e))?;
    Ok((continue_bb, ()))
}

/// Read the i8 tag byte at offset 0 of the envelope buffer.
fn read_envelope_tag<'ctx>(
    ctx: &EmitContext<'ctx>,
    envelope_ptr: PointerValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let i8_ty = ctx.context.i8_type();
    ctx.builder
        .build_load(i8_ty, envelope_ptr, "envelope_tag")
        .map(|v| v.into_int_value())
        .map_err(|e| inkwell_err("build_load envelope_tag", e))
}

/// Build the per-arm dispatch chain as a sequence of conditional
/// branches keyed on the envelope tag. Each matching arm gets a
/// dedicated "deserialize then branch to body" block so we can
/// share the GEP / payload-load logic without re-emitting it at
/// every comparison site.
///
/// Tags that no arm declares fall through to an `unreachable` —
/// the typecheck seal admits only declared shapes, so a runtime
/// envelope with an unknown tag indicates a runtime/wire-protocol
/// bug.
fn dispatch_arms<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: FunctionValue<'ctx>,
    envelope_ptr: PointerValue<'ctx>,
    tag_value: IntValue<'ctx>,
    arms: &[ReceiveArm],
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    for (index, arm) in arms.iter().enumerate() {
        let wire_byte = arm.tag.wire_byte();
        let is_match = ctx
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag_value,
                i8_ty.const_int(wire_byte as u64, false),
                &format!("is_arm_{index}"),
            )
            .map_err(|e| inkwell_err("build_int_compare arm tag", e))?;
        let arm_prelude_bb = ctx
            .context
            .append_basic_block(function, &format!("arm_{index}_prelude"));
        let next_bb = ctx
            .context
            .append_basic_block(function, &format!("arm_{index}_test"));
        ctx.builder
            .build_conditional_branch(is_match, arm_prelude_bb, next_bb)
            .map_err(|e| inkwell_err("build_conditional_branch arm dispatch", e))?;

        ctx.builder.position_at_end(arm_prelude_bb);
        deserialize_payload_into_local(ctx, envelope_ptr, arm)?;
        let body_bb = ctx.block_for(arm.body);
        ctx.builder
            .build_unconditional_branch(body_bb)
            .map_err(|e| inkwell_err("build_unconditional_branch arm body", e))?;

        ctx.builder.position_at_end(next_bb);
    }
    ctx.builder
        .build_unreachable()
        .map(|_| ())
        .map_err(|e| inkwell_err("build_unreachable receive fallthrough", e))
}

/// Load the typed payload out of `envelope_ptr` (offset 8) and
/// store it into the arm's payload local slot. The shape depends
/// on the [`ReceiveTag`]:
///
/// - `Lifecycle`: the runtime serializes the variant index as a
///   single byte at offset 8. We load it as the arm's enum-outer
///   (stamped by the layout pre-emit) using its full LLVM size,
///   so the trailing padding bytes the runtime allocator zeroed
///   stay quiet alongside the live tag byte.
/// - `Business`: the runtime serializes the unboxed business
///   message struct at offset 8 (today, a `Pair<M, Option<ReplyTo<R>>>`
///   produced by `Ref.cast` / `Ref.call`). We load the arm's
///   payload type directly.
fn deserialize_payload_into_local<'ctx>(
    ctx: &EmitContext<'ctx>,
    envelope_ptr: PointerValue<'ctx>,
    arm: &ReceiveArm,
) -> Result<(), LlvmError> {
    let i8_ty = ctx.context.i8_type();
    let payload_offset = i8_ty.const_int(ENVELOPE_PAYLOAD_OFFSET, false);
    let payload_ptr = unsafe {
        ctx.builder
            .build_gep(i8_ty, envelope_ptr, &[payload_offset], "payload_ptr")
            .map_err(|e| inkwell_err("build_gep payload_ptr", e))?
    };
    let payload_llvm_type = ir_basic_type(ctx, &arm.payload_type)?;
    let label = match arm.tag {
        ReceiveTag::Business => "business_payload",
        ReceiveTag::Lifecycle => "lifecycle_payload",
    };
    let payload = ctx
        .builder
        .build_load(payload_llvm_type, payload_ptr, label)
        .map_err(|e| inkwell_err("build_load receive payload", e))?;
    let slot = ctx.local_slot(arm.payload_local);
    ctx.builder
        .build_store(slot, payload)
        .map(|_| ())
        .map_err(|e| inkwell_err("build_store receive payload local", e))?;
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
    ctx.builder
        .build_store(alloca, value)
        .map_err(|e| inkwell_err(format_args!("build_store {label}"), e))?;
    let abi_size = ctx.layouts.target_data.get_abi_size(&llvm_type);
    let size = ctx.context.i64_type().const_int(abi_size, false);
    Ok((alloca, size))
}
