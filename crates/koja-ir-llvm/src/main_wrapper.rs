//! Synthesize the host `main` entry point. Two shapes, picked per
//! [`koja_ir::FunctionKind`] of the IR program's entry:
//!
//! - **Function entry** ([`emit_as_main`]) — legacy v1 `fn main`
//!   shape. Stamps `void __koja_user_main(i8*)` carrying the user
//!   body plus an `i64 main()` trampoline that registers the
//!   thunk as PID 1 and boots the scheduler. Always returns 0.
//! - **Process entry** ([`emit_process_entry_main`]) — PascalCase
//!   `Process<C, M, R>` shape. The IR program's entry function is
//!   a `ProcessEntryWrapper` already defined like any helper; this
//!   module only synthesizes the host `main` trampoline that
//!   builds the `C` argument, hands the wrapper to
//!   `koja_rt_spawn`, runs the scheduler via `koja_rt_main_done`,
//!   and returns whatever the wrapper stored into
//!   [`EXIT_CODE_SYMBOL`].
//!
//! The host `main`'s signature varies between shapes:
//! - Function entry stays at `i64 main()`.
//! - Process entry uses `i32 main(i32, i8**)` iff the entry state's
//!   config type is `List<String>` (so the trampoline can build a
//!   `List<String>` from argc/argv); otherwise it stays at
//!   `i32 main()` and zero-fills the config alloca.
//!
//! Running the user body inside a spawned process (PID 1) is what
//! lets `koja_rt_self()`, `Ref.call`, `Ref.cast`, and the rest of
//! the concurrency primitives work from `main` — they need a
//! `CURRENT_PID >= 1` thread-local and a real mailbox, both of
//! which the scheduler installs before invoking the spawned thunk.
//!
//! The [`__koja_app_name`](APP_NAME_SYMBOL) global lives here
//! because it's the same kind of "runtime convention" plumbing —
//! emitted on every compiled binary so the runtime archive's
//! panic handler links cleanly regardless of cgu partitioning.
//!
//! See [`koja-runtime/src/intrinsics.rs`](../../koja-runtime/src/intrinsics.rs)
//! for the runtime side of these conventions.

use std::collections::HashSet;

use inkwell::AddressSpace;
use inkwell::module::Linkage;
use koja_ir::{IRBasicBlock, IRBlockId, IRFunction, IRTerminator, IRType};

use crate::ctx::EmitContext;
use crate::emit::{self, ValueMap, inkwell_err};
use crate::error::LlvmError;
use crate::function::declare_blocks;
use crate::runtime::{
    declare_rt_build_argv_extern, declare_rt_main_done_extern, declare_rt_spawn_extern,
};
use crate::types::ir_basic_type;

const APP_NAME_SYMBOL: &str = "__koja_app_name";
const ENTRY_SYMBOL: &str = "main";
/// Module-level `i32` global the [`FunctionKind::ProcessEntryWrapper`]
/// body writes the entry process's exit code into; the synthesized
/// `main` trampoline returns its value after the scheduler joins.
/// Function-shaped entries never touch this global (they always
/// return 0 from `main`).
pub(crate) const EXIT_CODE_SYMBOL: &str = "__koja_exit_code";
/// Thunk that carries the user body. Single `i8*` parameter
/// (ignored) so it matches `koja_rt_spawn`'s `ProcessFn` typedef.
const USER_MAIN_SYMBOL: &str = "__koja_user_main";

/// Emit `__koja_app_name` as a null-terminated C-string constant.
/// The `koja-runtime` panic handler reads it for backtrace labels
/// (declared there as `extern static [c_char; 0]`); every
/// compiled binary defines it so the runtime archive links
/// cleanly regardless of codegen-unit partitioning.
pub(crate) fn emit_app_name_global(ctx: &EmitContext<'_>, app_name: &str) {
    let value = ctx.context.const_string(app_name.as_bytes(), true);
    let global = ctx
        .module
        .add_global(value.get_type(), None, APP_NAME_SYMBOL);
    global.set_initializer(&value);
    global.set_constant(true);
}

/// Emit the mutable `__koja_exit_code` (i32, init 0) global the
/// process-entry wrapper writes into. The Function-entry shape never
/// touches it; the Process-entry trampoline returns its value after
/// the scheduler joins.
pub(crate) fn emit_exit_code_global(ctx: &EmitContext<'_>) {
    if ctx.module.get_global(EXIT_CODE_SYMBOL).is_some() {
        return;
    }
    let i32_ty = ctx.context.i32_type();
    let global = ctx.module.add_global(i32_ty, None, EXIT_CODE_SYMBOL);
    global.set_initializer(&i32_ty.const_zero());
}

/// Emit `blocks` as a spawn-driven main pair:
///
/// 1. `void __koja_user_main(i8*)` carrying the user body.
/// 2. `i64 main()` trampoline that calls
///    `koja_rt_spawn(__koja_user_main, null, 0)` to register the
///    body as PID 1, then `koja_rt_main_done()` to boot the
///    scheduler (which runs until PID 1 dies), then `ret i64 0`.
///
/// The trailing block of `__koja_user_main` is always capped with
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

/// Define `void __koja_user_main(i8*)` carrying the user body.
/// The single pointer parameter is ignored — present only to match
/// `koja_rt_spawn`'s `ProcessFn` signature so the trampoline can
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

/// Synthesize the `main` trampoline for a Process-entry program.
/// The entry IR function is a [`FunctionKind::ProcessEntryWrapper`]
/// already declared+defined like any other helper; this trampoline
/// only handles host-side argv plumbing and the exit-code return.
///
/// Signature picks:
/// - `i32 main(i32, ptr)` iff the wrapper's config type lowers to
///   `IRType::List(String)` (the entry state is
///   `Process<List<String>, _, _>`). The body calls
///   `koja_rt_build_argv(argc, argv, &config_alloca)` to build a
///   `List<String>` in place before spawning.
/// - `i32 main()` otherwise. The config alloca is zero-initialized;
///   non-`List<String>` configs aren't reachable from `koja.toml`
///   today (Process configs that aren't `List<String>` only make
///   sense for spawn-from-user-code, not the project entry), but
///   the shape leaves the door open.
pub(crate) fn emit_process_entry_main<'ctx>(
    ctx: &EmitContext<'ctx>,
    entry: &IRFunction,
) -> Result<(), LlvmError> {
    let config_type = entry.params.first().map(|p| &p.ty).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "LLVM emit: process entry wrapper `{}` has no config parameter",
            entry.symbol,
        ))
    })?;
    let argv_shaped = matches!(
        config_type,
        IRType::List(element) if matches!(**element, IRType::String)
    );

    let i32_ty = ctx.context.i32_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let signature = if argv_shaped {
        i32_ty.fn_type(&[i32_ty.into(), ptr_ty.into()], false)
    } else {
        i32_ty.fn_type(&[], false)
    };
    let main_fn = ctx
        .module
        .add_function(ENTRY_SYMBOL, signature, Some(Linkage::External));

    let entry_bb = ctx.context.append_basic_block(main_fn, "entry");
    ctx.builder.position_at_end(entry_bb);

    let config_llvm_type = ir_basic_type(ctx, config_type)?;
    let config_alloca = ctx
        .builder
        .build_alloca(config_llvm_type, "entry_config")
        .map_err(|e| inkwell_err("build_alloca entry_config", e))?;
    if argv_shaped {
        let argc = main_fn.get_nth_param(0).ok_or_else(|| {
            LlvmError::Codegen("process entry main missing argc parameter".to_string())
        })?;
        let argv = main_fn.get_nth_param(1).ok_or_else(|| {
            LlvmError::Codegen("process entry main missing argv parameter".to_string())
        })?;
        let build_argv = declare_rt_build_argv_extern(ctx);
        ctx.builder
            .build_call(
                build_argv,
                &[argc.into(), argv.into(), config_alloca.into()],
                "",
            )
            .map_err(|e| inkwell_err("call koja_rt_build_argv", e))?;
    } else {
        ctx.builder
            .build_store(config_alloca, config_llvm_type.const_zero())
            .map_err(|e| inkwell_err("zero-init entry config", e))?;
    }

    let wrapper_fn = ctx.declared_function(&entry.symbol).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "LLVM emit: process entry wrapper `{}` not declared before main trampoline emit",
            entry.symbol,
        ))
    })?;
    let wrapper_ptr = wrapper_fn.as_global_value().as_pointer_value();
    let config_size = ctx.context.i64_type().const_int(
        ctx.layouts.target_data.get_abi_size(&config_llvm_type),
        false,
    );
    let spawn_fn = declare_rt_spawn_extern(ctx);
    ctx.builder
        .build_call(
            spawn_fn,
            &[wrapper_ptr.into(), config_alloca.into(), config_size.into()],
            "",
        )
        .map_err(|e| inkwell_err("call koja_rt_spawn (process entry main)", e))?;
    let main_done = declare_rt_main_done_extern(ctx);
    ctx.builder
        .build_call(main_done, &[], "")
        .map_err(|e| inkwell_err("call koja_rt_main_done (process entry main)", e))?;

    let exit_global = ctx.module.get_global(EXIT_CODE_SYMBOL).ok_or_else(|| {
        LlvmError::Codegen(format!(
            "LLVM emit: `{EXIT_CODE_SYMBOL}` global not declared before main trampoline emit",
        ))
    })?;
    let exit_value = ctx
        .builder
        .build_load(i32_ty, exit_global.as_pointer_value(), "exit_code")
        .map_err(|e| inkwell_err("load __koja_exit_code", e))?
        .into_int_value();
    ctx.builder
        .build_return(Some(&exit_value))
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return process entry main", e))
}

/// Define `i64 main()` as a minimal trampoline that hands the
/// user-body thunk to the runtime as the entry process.
///
/// `koja_rt_spawn(__koja_user_main, null, 0)` registers the thunk
/// as PID 1 with a zero-byte config (the thunk ignores the
/// pointer); `koja_rt_main_done()` boots the I/O reactor + worker
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
            "LLVM emit: `{USER_MAIN_SYMBOL}` not declared before main trampoline emit",
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
        .map_err(|e| inkwell_err("call koja_rt_spawn (main trampoline)", e))?;
    let main_done = declare_rt_main_done_extern(ctx);
    ctx.builder
        .build_call(main_done, &[], "")
        .map_err(|e| inkwell_err("call koja_rt_main_done (main trampoline)", e))?;
    ctx.builder
        .build_return(Some(&zero_i64))
        .map(|_| ())
        .map_err(|e| inkwell_err("build_return main trampoline", e))
}

/// Cap the trailing block of `__koja_user_main` with `ret void`.
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
                    "LLVM expects exactly one reachable Return-terminated block in `main`"
                        .to_string(),
                ));
            }
            found = Some(block.id);
        }
    }
    found.ok_or_else(|| {
        LlvmError::Codegen(
            "LLVM expects at least one reachable Return-terminated block in `main`".to_string(),
        )
    })
}
