//! Synthesize the host `i64 main()` entry point.
//!
//! Two-function shape, mirroring v1's `expo-codegen` convention:
//!
//! - `void __expo_user_main(i8*)` — a spawn-wrapper-shaped thunk
//!   carrying the user body. Always terminates with `ret void`;
//!   the trailing expression's value (if any) is computed for its
//!   side effects and then discarded. Single `i8*` parameter
//!   (ignored) for ABI compatibility with `expo_rt_spawn`'s
//!   `ProcessFn` typedef.
//! - `i64 main()` — minimal trampoline: `expo_rt_spawn(
//!   __expo_user_main, null, 0)` registers the body as PID 1,
//!   `expo_rt_main_done()` boots the scheduler and runs until
//!   PID 1 dies, then `ret i64 0`.
//!
//! Running the user body inside a spawned process (PID 1) is what
//! lets `expo_rt_self()`, `Ref.call`, `Ref.cast`, and the rest of
//! the concurrency primitives work from `main` — they need a
//! `CURRENT_PID >= 1` thread-local and a real mailbox, both of
//! which the scheduler installs before invoking the spawned thunk.
//!
//! Scripts and programs always exit 0 on normal completion. To
//! surface a non-zero exit code the user must call an explicit
//! exit intrinsic; the trailing expression's value is not
//! examined here.
//!
//! The [`__expo_app_name`](APP_NAME_SYMBOL) global lives here
//! because it's the same kind of "runtime convention" plumbing —
//! emitted on every alpha-compiled binary so the runtime archive's
//! panic handler links cleanly regardless of cgu partitioning.
//!
//! See [`expo-runtime/src/alpha.rs`](../../expo-runtime/src/alpha.rs)
//! for the runtime side of these conventions.

use std::collections::HashSet;

use expo_alpha_ir::{IRBasicBlock, IRBlockId, IRTerminator};
use inkwell::AddressSpace;
use inkwell::module::Linkage;

use crate::ctx::EmitContext;
use crate::emit::{self, ValueMap, inkwell_err};
use crate::error::LlvmError;
use crate::function::declare_blocks;
use crate::runtime::{declare_rt_main_done_extern, declare_rt_spawn_extern};

const APP_NAME_SYMBOL: &str = "__expo_app_name";
const ENTRY_SYMBOL: &str = "main";
/// Thunk that carries the user body. Single `i8*` parameter
/// (ignored) so it matches `expo_rt_spawn`'s `ProcessFn` typedef.
const USER_MAIN_SYMBOL: &str = "__expo_user_main";

/// Emit `__expo_app_name` as a null-terminated C-string constant.
/// The `expo-runtime` panic handler reads it for backtrace labels
/// (declared there as `extern static [c_char; 0]`); every
/// alpha-compiled binary defines it so the runtime archive links
/// cleanly regardless of codegen-unit partitioning.
pub(crate) fn emit_app_name_global(ctx: &EmitContext<'_>, app_name: &str) {
    let value = ctx.context.const_string(app_name.as_bytes(), true);
    let global = ctx
        .module
        .add_global(value.get_type(), None, APP_NAME_SYMBOL);
    global.set_initializer(&value);
    global.set_constant(true);
}

/// Emit `blocks` as a spawn-driven main pair:
///
/// 1. `void __expo_user_main(i8*)` carrying the user body.
/// 2. `i64 main()` trampoline that calls
///    `expo_rt_spawn(__expo_user_main, null, 0)` to register the
///    body as PID 1, then `expo_rt_main_done()` to boot the
///    scheduler (which runs until PID 1 dies), then `ret i64 0`.
///
/// The trailing block of `__expo_user_main` is always capped with
/// `ret void` — the user body's trailing value is computed (for
/// its side effects) and discarded. Empty bodies are illegal
/// (sealed IR guarantees at least one block), and the final IR
/// block must end in `Return`. The seal pass admits other
/// terminators for non-trailing blocks.
pub(crate) fn emit_as_main<'ctx>(
    ctx: &EmitContext<'ctx>,
    blocks: &[IRBasicBlock],
) -> Result<(), LlvmError> {
    define_user_main(ctx, blocks)?;
    define_main_trampoline(ctx)
}

/// Define `void __expo_user_main(i8*)` carrying the user body.
/// The single pointer parameter is ignored — present only to match
/// `expo_rt_spawn`'s `ProcessFn` signature so the trampoline can
/// hand the function pointer over directly.
fn define_user_main<'ctx>(
    ctx: &EmitContext<'ctx>,
    blocks: &[IRBasicBlock],
) -> Result<(), LlvmError> {
    let ptr_type = ctx.context.ptr_type(AddressSpace::default());
    let signature = ctx.context.void_type().fn_type(&[ptr_type.into()], false);
    let function = ctx
        .module
        .add_function(USER_MAIN_SYMBOL, signature, Some(Linkage::External));
    // The script-mode body is its own function from a slot-identity
    // perspective; flush any stragglers from a prior compile or
    // helper so `LocalDecl` registers cleanly here.
    ctx.reset_locals();
    let block_map = declare_blocks(ctx, function, blocks);
    let reachable = emit::reachable_blocks(blocks);
    let return_block_id = find_return_block(blocks, &reachable)?;

    let mut values: ValueMap<'ctx> = ValueMap::new();
    let phi_map = emit::declare_block_param_phis(ctx, blocks, &block_map, &mut values)?;
    ctx.set_block_map(block_map.clone());
    let result = (|| -> Result<(), LlvmError> {
        for block in blocks {
            if !reachable.contains(&block.id) {
                // Same boundary stand-in as `define_function`: blocks the
                // CFG can't reach get `unreachable` so we never try to
                // materialize their (impossible-to-reach) value reads.
                emit::emit_unreachable_terminator(ctx, block.id, &block_map)?;
                continue;
            }
            let llvm_block = block_map[&block.id];
            ctx.builder.position_at_end(llvm_block);
            if block.id == return_block_id {
                let (next_values, _terminator) =
                    emit::emit_instructions(ctx, block, std::mem::take(&mut values))?;
                values = next_values;
                emit_user_main_return(ctx)?;
            } else {
                emit::emit_block(ctx, block, &block_map, &phi_map, &mut values)?;
            }
        }
        Ok(())
    })();
    ctx.clear_block_map();
    result
}

/// Define `i64 main()` as a minimal trampoline that hands the
/// user-body thunk to the runtime as the entry process.
///
/// `expo_rt_spawn(__expo_user_main, null, 0)` registers the thunk
/// as PID 1 with a zero-byte config (the thunk ignores the
/// pointer); `expo_rt_main_done()` boots the I/O reactor + worker
/// pool and runs the scheduling loop until PID 1 dies. Returning
/// `0` from `main` after the scheduler joins keeps the host exit
/// status at success unless the runtime itself exited with a
/// non-zero status (lifecycle paths handle that on their own).
fn define_main_trampoline<'ctx>(ctx: &EmitContext<'ctx>) -> Result<(), LlvmError> {
    let i64_type = ctx.context.i64_type();
    let ptr_type = ctx.context.ptr_type(AddressSpace::default());
    let signature = i64_type.fn_type(&[], false);
    let function = ctx
        .module
        .add_function(ENTRY_SYMBOL, signature, Some(Linkage::External));
    let entry = ctx.context.append_basic_block(function, "entry");
    ctx.builder.position_at_end(entry);

    let user_main_fn = ctx.module.get_function(USER_MAIN_SYMBOL).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "alpha LLVM emit: `{USER_MAIN_SYMBOL}` not declared before main trampoline emit",
        ))
    })?;
    let user_main_ptr = user_main_fn.as_global_value().as_pointer_value();
    let null_ptr = ptr_type.const_null();
    let zero_i64 = i64_type.const_int(0, false);
    let spawn_fn = declare_rt_spawn_extern(ctx);
    ctx.builder
        .build_call(
            spawn_fn,
            &[user_main_ptr.into(), null_ptr.into(), zero_i64.into()],
            "",
        )
        .map_err(|e| inkwell_err("call expo_rt_spawn (main trampoline)", e))?;
    let main_done = declare_rt_main_done_extern(ctx);
    ctx.builder
        .build_call(main_done, &[], "")
        .map_err(|e| inkwell_err("call expo_rt_main_done (main trampoline)", e))?;
    ctx.builder
        .build_return(Some(&zero_i64))
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return main trampoline", e))
}

/// Cap the trailing block of `__expo_user_main` with `ret void`.
/// The user body's `Return` terminator's value (if any) has
/// already been emitted by [`emit::emit_instructions`]; we discard
/// it because scripts and programs always exit 0 on normal
/// completion.
///
/// Skips the cap when the host block is already terminated —
/// `IRInstruction::Receive` ends the block with its dispatcher
/// branch, so emitting `ret void` after would be a duplicate
/// terminator.
fn emit_user_main_return<'ctx>(ctx: &EmitContext<'ctx>) -> Result<(), LlvmError> {
    if let Some(insert_block) = ctx.builder.get_insert_block()
        && insert_block.get_terminator().is_some()
    {
        return Ok(());
    }
    ctx.builder
        .build_return(None)
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return for user_main", e))
}

/// The [`IRBlockId`] of the unique *reachable* block ending in
/// `Return`. Today's slice produces exactly one reachable
/// `Return`-terminated block per function (the merge block of an
/// `if` / `unless` falls through to it via `Branch`); divergent
/// if/else's may synthesize an unreachable merge whose `Return`
/// reads an unmaterialized `BlockParam` — those don't count and
/// are filtered out via `reachable`. A missing or duplicate
/// reachable `Return` is a lowering bug we surface as a codegen
/// error.
fn find_return_block(
    blocks: &[IRBasicBlock],
    reachable: &HashSet<IRBlockId>,
) -> Result<IRBlockId, LlvmError> {
    let mut found: Option<IRBlockId> = None;
    for block in blocks {
        if !reachable.contains(&block.id) {
            continue;
        }
        if matches!(block.terminator, IRTerminator::Return { .. }) {
            if found.is_some() {
                return Err(LlvmError::Codegen(
                    "alpha LLVM expects exactly one reachable Return-terminated block in `main`"
                        .to_string(),
                ));
            }
            found = Some(block.id);
        }
    }
    found.ok_or_else(|| {
        LlvmError::Codegen(
            "alpha LLVM expects at least one reachable Return-terminated block in `main`"
                .to_string(),
        )
    })
}
