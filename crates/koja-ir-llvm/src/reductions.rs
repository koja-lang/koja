//! Reduction-budget emission, covering the cooperative-preemption
//! spend behind [`koja_ir::IRInstruction::YieldCheck`] and the
//! per-process seed. The strategy is selected by host architecture.
//!
//! On aarch64 the budget lives in the callee-saved register `x26`,
//! removed from LLVM's allocator by the `+reserve-x26` target feature.
//! A check is a register decrement plus a signed `<= 0` branch, with
//! no memory traffic. Because the register is callee-saved, it rides
//! untouched through runtime calls and the context switch, worker
//! migration included. Fresh grants flow in by return value only.
//! `koja_rt_yield_check` returns the next quantum's grant and process
//! entries call `koja_rt_reductions_grant` once. Return registers
//! survive the Rust frames between compiled code and the switch, while
//! values poked into saved-register stack slots do not, because those
//! frames save and restore the register themselves.
//!
//! The register scheme needs LLVM 21 or newer. Since LLVM 21,
//! user-reserved registers are never spilled or restored as callee
//! saves (matching GCC), so writes through `llvm.write_register`
//! persist across returns. Older LLVM spills a modified reserved
//! register like any other callee save, and on Darwin also pads
//! compact-unwind pairs with it, so epilogue restores would roll back
//! every spend made by a function and its callees and deep recursion
//! would never yield.
//!
//! On x86_64 LLVM cannot reserve a GPR yet (`+reserve-r8..r15` landed
//! after the LLVM 22 branch), so the budget stays in the C
//! thread-local `koja_reductions_left` that the scheduler seeds each
//! quantum. A check is a load / decrement / store that calls
//! `koja_rt_yield_check` at zero.
//!
//! The register scheme leans on a check-placement invariant. Only
//! `FunctionKind::Regular` bodies carry `YieldCheck`s (glue kinds are
//! skipped by `koja-ir`'s yield pass), and the runtime invokes nothing
//! but glue outside process stacks (envelope drop glue on scheduler
//! stacks, for example). A check in runtime-invoked code would
//! decrement whatever the interrupted Rust frame keeps in `x26`.

use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::intrinsics::Intrinsic;
use inkwell::targets::TargetMachine;
use inkwell::values::{BasicMetadataValueEnum, FunctionValue, IntValue};

use crate::ctx::EmitContext;
use crate::error::{IceExt, LlvmError};
use crate::runtime::{
    declare_rt_reductions_grant_extern, declare_rt_yield_check_extern, reductions_counter_global,
};

/// Callee-saved register holding the reduction budget on aarch64.
/// x18 is the platform register and x19 carries the process-start
/// entry pointer, so the pick avoids both by a margin.
const BUDGET_REGISTER: &str = "x26";

/// Whether the host keeps the budget in [`BUDGET_REGISTER`] instead of
/// the thread-local. Compile-time constant: the backend only targets
/// the host triple.
fn budget_register_enabled() -> bool {
    cfg!(target_arch = "aarch64")
}

/// Host CPU feature string for target-machine construction, with the
/// budget-register reservation appended where the register scheme is
/// active. Both target machines (object emission and layout) must use
/// this so compiled code never allocates the reserved register.
pub(crate) fn host_cpu_features() -> String {
    let mut features = TargetMachine::get_host_cpu_features().to_string();
    if budget_register_enabled() {
        if !features.is_empty() {
            features.push(',');
        }
        features.push_str("+reserve-");
        features.push_str(BUDGET_REGISTER);
    }
    features
}

/// Emit [`koja_ir::IRInstruction::YieldCheck`], spending one reduction
/// and branching into `koja_rt_yield_check` when the budget is
/// exhausted. The common case stays call-free on both strategies.
pub(crate) fn emit_yield_check(ctx: &EmitContext<'_>) -> Result<(), LlvmError> {
    if budget_register_enabled() {
        emit_register_yield_check(ctx)
    } else {
        emit_tls_yield_check(ctx)
    }
}

/// Seed the budget register at a compiled process entry (spawn
/// wrappers and the script user-main thunk). No-op on the
/// thread-local strategy, where the scheduler seeds per quantum.
pub(crate) fn emit_budget_seed(ctx: &EmitContext<'_>) -> Result<(), LlvmError> {
    if !budget_register_enabled() {
        return Ok(());
    }
    let grant_fn = declare_rt_reductions_grant_extern(ctx);
    let grant = ctx
        .call_basic(grant_fn, &[], "budget_grant")?
        .into_int_value();
    write_budget_register(ctx, widen_grant(ctx, grant)?)
}

/// Register strategy. Decrement [`BUDGET_REGISTER`] and branch to the
/// slow path on a signed `<= 0`. The signed compare (instead of an
/// exact zero test) makes a clobbered or zero seed yield immediately
/// instead of wrapping into a never-yielding budget. The slow path
/// reseeds from `koja_rt_yield_check`'s returned grant.
fn emit_register_yield_check(ctx: &EmitContext<'_>) -> Result<(), LlvmError> {
    let (yield_bb, continue_bb) = yield_check_blocks(ctx)?;

    let i64_ty = ctx.context.i64_type();
    let decremented = spend_budget_register(ctx)?;
    let exhausted = ctx
        .builder
        .build_int_compare(
            IntPredicate::SLE,
            decremented,
            i64_ty.const_zero(),
            "reductions_out",
        )
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(exhausted, yield_bb, continue_bb)
        .or_ice()?;

    ctx.builder.position_at_end(yield_bb);
    let yield_check_fn = declare_rt_yield_check_extern(ctx);
    let fresh = ctx
        .call_basic(yield_check_fn, &[], "budget_grant")?
        .into_int_value();
    write_budget_register(ctx, widen_grant(ctx, fresh)?)?;
    ctx.builder
        .build_unconditional_branch(continue_bb)
        .or_ice()?;

    ctx.builder.position_at_end(continue_bb);
    Ok(())
}

/// Thread-local strategy. Decrement the per-worker
/// `koja_reductions_left` and branch into `koja_rt_yield_check` when it
/// reaches zero. The common case is a load / sub / store with no call.
fn emit_tls_yield_check(ctx: &EmitContext<'_>) -> Result<(), LlvmError> {
    let (yield_bb, continue_bb) = yield_check_blocks(ctx)?;

    let i32_ty = ctx.context.i32_type();
    let counter = reductions_counter_global(ctx).as_pointer_value();
    let current = ctx
        .builder
        .build_load(i32_ty, counter, "reductions")
        .or_ice()?
        .into_int_value();
    let decremented = ctx
        .builder
        .build_int_sub(current, i32_ty.const_int(1, false), "reductions_next")
        .or_ice()?;
    ctx.builder.build_store(counter, decremented).or_ice()?;

    let exhausted = ctx
        .builder
        .build_int_compare(
            IntPredicate::EQ,
            decremented,
            i32_ty.const_zero(),
            "reductions_out",
        )
        .or_ice()?;
    ctx.builder
        .build_conditional_branch(exhausted, yield_bb, continue_bb)
        .or_ice()?;

    ctx.builder.position_at_end(yield_bb);
    let yield_check_fn = declare_rt_yield_check_extern(ctx);
    // The returned grant only matters to the register strategy.
    ctx.builder.build_call(yield_check_fn, &[], "").or_ice()?;
    ctx.builder
        .build_unconditional_branch(continue_bb)
        .or_ice()?;

    ctx.builder.position_at_end(continue_bb);
    Ok(())
}

/// Append the `yield_slow` / `yield_cont` block pair to the current
/// function.
fn yield_check_blocks<'ctx>(
    ctx: &EmitContext<'ctx>,
) -> Result<(BasicBlock<'ctx>, BasicBlock<'ctx>), LlvmError> {
    let host_block = ctx.builder.get_insert_block().ok_or_else(|| {
        LlvmError::Codegen("LLVM emit: YieldCheck emitted with no insertion block".to_string())
    })?;
    let function = host_block.get_parent().ok_or_else(|| {
        LlvmError::Codegen("LLVM emit: YieldCheck's host block has no parent function".to_string())
    })?;
    let yield_bb = ctx.context.append_basic_block(function, "yield_slow");
    let continue_bb = ctx.context.append_basic_block(function, "yield_cont");
    Ok((yield_bb, continue_bb))
}

/// Zero-extend a `u32` grant to the 64-bit register width.
fn widen_grant<'ctx>(
    ctx: &EmitContext<'ctx>,
    grant: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    ctx.builder
        .build_int_z_extend(grant, ctx.context.i64_type(), "budget")
        .or_ice()
}

/// Spend one reduction, returning the decremented budget.
fn spend_budget_register<'ctx>(ctx: &EmitContext<'ctx>) -> Result<IntValue<'ctx>, LlvmError> {
    let read = register_intrinsic(ctx, "llvm.read_register")?;
    let current = ctx
        .call_basic(read, &[budget_register_name(ctx)], "reductions")?
        .into_int_value();
    let decremented = ctx
        .builder
        .build_int_sub(
            current,
            ctx.context.i64_type().const_int(1, false),
            "reductions_next",
        )
        .or_ice()?;
    write_budget_register(ctx, decremented)?;
    Ok(decremented)
}

/// Overwrite [`BUDGET_REGISTER`] with a fresh grant.
fn write_budget_register<'ctx>(
    ctx: &EmitContext<'ctx>,
    value: IntValue<'ctx>,
) -> Result<(), LlvmError> {
    let write = register_intrinsic(ctx, "llvm.write_register")?;
    ctx.builder
        .build_call(write, &[budget_register_name(ctx), value.into()], "")
        .or_ice()?;
    Ok(())
}

/// Declare (or look up) the i64 overload of `llvm.read_register` /
/// `llvm.write_register`.
fn register_intrinsic<'ctx>(
    ctx: &EmitContext<'ctx>,
    name: &str,
) -> Result<FunctionValue<'ctx>, LlvmError> {
    Intrinsic::find(name)
        .and_then(|intrinsic| {
            intrinsic.get_declaration(&ctx.module, &[ctx.context.i64_type().into()])
        })
        .ok_or_else(|| LlvmError::Codegen(format!("LLVM emit: `{name}` intrinsic unavailable")))
}

/// The `metadata !{!"x26"}` operand the register intrinsics take,
/// naming [`BUDGET_REGISTER`].
fn budget_register_name<'ctx>(ctx: &EmitContext<'ctx>) -> BasicMetadataValueEnum<'ctx> {
    let name = ctx.context.metadata_string(BUDGET_REGISTER);
    ctx.context.metadata_node(&[name.into()]).into()
}
