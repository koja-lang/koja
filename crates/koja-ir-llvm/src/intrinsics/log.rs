//! `LogRuntime.*` `@intrinsic` emitters, the runtime plumbing under
//! the stdlib `Log` module. Each is backed by a `koja_rt_log_*` extern
//! declared in [`crate::runtime`] and implemented in
//! `koja-runtime-posix`.
//!
//! The configuration type is opaque here. It rides the `IRFunction`
//! signature, and crosses into the runtime the way a span record does
//! in [`super::trace`]: spilled to a stack slot and handed over as
//! bytes plus length plus glue. `configure` also hands over a deep-copy
//! shim, since the runtime clones the stored bytes at `spawn` and on
//! every `config` read.

use inkwell::IntPredicate;
use inkwell::values::FunctionValue;
use koja_ir::{IRFunction, LogRuntimeMethod};

use super::process::{payload_copy_glue, payload_drop_glue};
use super::trace::{emit_fill_option, spill_record};
use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::intrinsics::util::{nth_int, nth_param, nth_param_type};
use crate::runtime::{
    declare_rt_log_config_extern, declare_rt_log_configure_extern, declare_rt_log_enter_extern,
    declare_rt_log_leave_extern, declare_rt_log_level_extern,
};

pub(super) fn emit_log_runtime<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: LogRuntimeMethod,
) -> Result<(), LlvmError> {
    match method {
        LogRuntimeMethod::Config => emit_config(ctx, function, llvm_function),
        LogRuntimeMethod::Configure => emit_configure(ctx, function, llvm_function),
        LogRuntimeMethod::Enter => emit_enter(ctx),
        LogRuntimeMethod::Leave => emit_leave(ctx),
        LogRuntimeMethod::Level => emit_level(ctx),
    }
}

/// `LogRuntime.level() -> Int`: `koja_rt_log_level()`.
fn emit_level(ctx: &EmitContext<'_>) -> Result<(), LlvmError> {
    let level_fn = declare_rt_log_level_extern(ctx);
    let level = ctx.call_basic(level_fn, &[], "log_level")?;
    ctx.builder.build_return(Some(&level)).or_ice().map(|_| ())
}

/// `LogRuntime.configure(config, level: Int)`: spill the configuration
/// and hand it to `koja_rt_log_configure` with its drop and copy shims
/// and the floor word.
fn emit_configure<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let config_value = nth_param(function, llvm_function, 0, "config");
    let level = nth_int(function, llvm_function, 1, "level");
    let (slot, size) = spill_record(ctx, config_value, "log_config")?;
    let config_type = nth_param_type(function, 0);
    let drop_glue = payload_drop_glue(ctx, config_type)?;
    let copy_glue = payload_copy_glue(ctx, config_type)?;
    let configure_fn = declare_rt_log_configure_extern(ctx);
    ctx.builder
        .build_call(
            configure_fn,
            &[
                slot.into(),
                size.into(),
                drop_glue.into(),
                copy_glue.into(),
                level.into(),
            ],
            "",
        )
        .or_ice()?;
    ctx.builder.build_return(None).or_ice().map(|_| ())
}

/// `LogRuntime.config() -> Option<config>`: let `koja_rt_log_config`
/// fill a stack slot with a deep copy of the stored configuration. A
/// `0` status loads the slot into `Option.Some`, `-1` yields
/// `Option.None`.
fn emit_config<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let config_fn = declare_rt_log_config_extern(ctx);
    emit_fill_option(ctx, function, llvm_function, "LogRuntime.config", config_fn)
}

/// `LogRuntime.enter() -> Bool`: compare `koja_rt_log_enter()` against
/// zero and return the `i1` result. `1` means the calling process was
/// not already running its handlers.
fn emit_enter(ctx: &EmitContext<'_>) -> Result<(), LlvmError> {
    let enter_fn = declare_rt_log_enter_extern(ctx);
    let status = ctx.call_basic(enter_fn, &[], "log_enter")?.into_int_value();
    let zero = ctx.context.i64_type().const_zero();
    let entered = ctx
        .builder
        .build_int_compare(IntPredicate::NE, status, zero, "entered")
        .or_ice()?;
    ctx.builder
        .build_return(Some(&entered))
        .or_ice()
        .map(|_| ())
}

/// `LogRuntime.leave()`: `koja_rt_log_leave()`.
fn emit_leave(ctx: &EmitContext<'_>) -> Result<(), LlvmError> {
    let leave_fn = declare_rt_log_leave_extern(ctx);
    ctx.builder.build_call(leave_fn, &[], "").or_ice()?;
    ctx.builder.build_return(None).or_ice().map(|_| ())
}
