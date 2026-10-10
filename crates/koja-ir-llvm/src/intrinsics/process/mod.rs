//! `Ref<M, R>`, `ReplyTo<R>`, and `Process` `@intrinsic` emitters,
//! each backed by a per-method `koja_rt_*` extern declared in
//! [`crate::runtime`] and implemented in `koja-runtime-posix`. This
//! module dispatches per method and reads the `self` receiver.
//! [`ref_methods`] holds `Ref.*`, [`process_methods`] holds
//! `Process.*`, [`reply_to_methods`] holds `ReplyTo.*`, and
//! [`envelope`] builds the message envelopes and their drop shims.

mod envelope;
mod process_methods;
mod ref_methods;
mod reply_to_methods;

use inkwell::values::{FunctionValue, IntValue};
use koja_ir::{IRFunction, ProcessMethod, RefMethod, ReplyToMethod};

use crate::ctx::EmitContext;
use crate::error::LlvmError;
use crate::intrinsics::util::{extract_int, nth_int, nth_struct};

pub(crate) use envelope::{payload_copy_glue, payload_drop_glue};

pub(super) fn emit_process<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: ProcessMethod,
) -> Result<(), LlvmError> {
    match method {
        ProcessMethod::Context => process_methods::emit_context(ctx, function),
        ProcessMethod::Demonitor => process_methods::emit_demonitor(ctx, function, llvm_function),
        ProcessMethod::Monitor => process_methods::emit_monitor(ctx, function, llvm_function),
        ProcessMethod::Parent => process_methods::emit_parent(ctx, function),
    }
}

pub(super) fn emit_ref<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: RefMethod,
) -> Result<(), LlvmError> {
    match method {
        RefMethod::AliveQ => ref_methods::emit_alive(ctx, function, llvm_function),
        RefMethod::Call => ref_methods::emit_call(ctx, function, llvm_function),
        RefMethod::Cast => ref_methods::emit_send_envelope(ctx, function, llvm_function, None),
        RefMethod::Kill => ref_methods::emit_kill(ctx, function, llvm_function),
        RefMethod::SelfRef => ref_methods::emit_self_ref(ctx, function),
        RefMethod::SendAfter => {
            let delay = nth_int(function, llvm_function, 2, "delay");
            ref_methods::emit_send_envelope(ctx, function, llvm_function, Some(delay))
        }
        RefMethod::Signal => ref_methods::emit_signal(ctx, function, llvm_function),
    }
}

pub(super) fn emit_reply_to<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    method: ReplyToMethod,
) -> Result<(), LlvmError> {
    match method {
        ReplyToMethod::Send => reply_to_methods::emit_reply_send(ctx, function, llvm_function),
    }
}

/// Pull the i64 pid out of the `self` parameter (always param #0
/// for these methods). `Ref<M, R>` lays out as `{ i64 id }` and
/// `ReplyTo<R>` as `{ i64 id, i64 token }`, so the pid is field 0
/// of both.
fn pid_from_self<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    function: &IRFunction,
) -> Result<IntValue<'ctx>, LlvmError> {
    self_field(ctx, llvm_function, function, 0, "pid")
}

fn self_field<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    function: &IRFunction,
    index: u32,
    name: &str,
) -> Result<IntValue<'ctx>, LlvmError> {
    let self_struct = nth_struct(function, llvm_function, 0, "self");
    extract_int(ctx, self_struct, index, name)
}

/// Pull the i64 correlation token out of a `ReplyTo<R>` `self`
/// parameter (field 1, see [`pid_from_self`] for the layout).
fn token_from_self<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    function: &IRFunction,
) -> Result<IntValue<'ctx>, LlvmError> {
    self_field(ctx, llvm_function, function, 1, "token")
}
