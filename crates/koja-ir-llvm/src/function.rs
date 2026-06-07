//! Non-entry function emission: declare an LLVM function for an
//! [`IRFunction`] (no body), then define its body once every helper
//! has been declared. The two-phase declare-then-define pattern lets
//! mutually-recursive calls resolve through the
//! `IRSymbol -> FunctionValue` index on [`EmitContext`] (populated
//! at declare time) before either body has been walked.
//!
//! The entry function (`main`) follows a different shape — see
//! [`crate::main_wrapper::emit_as_main`] for the auto-print
//! scaffolding.

use std::collections::BTreeMap;

use inkwell::AddressSpace;
use inkwell::basic_block::BasicBlock;
use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, FunctionType};
use inkwell::values::FunctionValue;
use koja_ir::{
    FunctionKind, IRBasicBlock, IRBlockId, IRFunction, IRInstruction, IRType,
    function_has_tail_call,
};

use crate::ctx::{ClosureFrame, EmitContext, TcoFrame};
use crate::emit::heap_layout::is_heap_leaf;
use crate::emit::process::{emit_process_entry_wrapper_body, emit_spawn_wrapper_body};
use crate::emit::{self, BlockMap, ValueMap, inkwell_err};
use crate::error::LlvmError;
use crate::intrinsics;
use crate::types::{closure_body_signature, env_struct_type, ir_basic_type, value_basic_type};

/// Declare an LLVM function for `function`. The signature mirrors
/// the IR exactly: each [`koja_ir::IRFunctionParam::ty`]
/// becomes its LLVM basic type and the return type does the same.
/// `Unit` returns / params surface as feature-gap diagnostics
/// through [`ir_basic_type`].
///
/// The LLVM symbol name is picked per-kind:
///
/// - `Regular` / `Intrinsic` declare under
///   [`koja_ir::IRSymbol::mangled`] (the internal form).
/// - `Extern(attrs)` declares under
///   [`koja_ir::IRExternAttrs::link_name`] when present, or
///   the function's bare last segment otherwise (`TestApp.cosf` →
///   `cosf`). The IRSymbol stays the call-site's resolution key,
///   regardless of the LLVM name.
///
/// Idempotent: if `module.get_function(name)` already exists for a
/// previously-seen `link_name` (multiple decls of the same C
/// symbol), reuse the existing handle rather than colliding. The
/// returned [`FunctionValue`] is also registered in the
/// `IRSymbol -> FunctionValue` index on `ctx` so call sites can
/// resolve through [`EmitContext::declared_function`] without
/// re-deriving the alias name.
pub(crate) fn declare_function<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
) -> Result<FunctionValue<'ctx>, LlvmError> {
    let signature = function_signature(ctx, function)?;
    let llvm_name = match &function.kind {
        FunctionKind::Closure { .. } => function.symbol.mangled().to_string(),
        FunctionKind::Extern(attrs) => attrs
            .link_name
            .clone()
            .unwrap_or_else(|| function.symbol.last_segment().to_string()),
        FunctionKind::CloneGlue
        | FunctionKind::DropGlue
        | FunctionKind::Intrinsic(_)
        | FunctionKind::ProcessEntryWrapper { .. }
        | FunctionKind::Regular
        | FunctionKind::SpawnWrapper { .. } => function.symbol.mangled().to_string(),
    };
    let llvm_function = match ctx.module.get_function(&llvm_name) {
        Some(existing) => existing,
        None => ctx
            .module
            .add_function(&llvm_name, signature, Some(Linkage::External)),
    };
    ctx.register_declared_function(function.symbol.clone(), llvm_function);
    Ok(llvm_function)
}

fn function_signature<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
) -> Result<FunctionType<'ctx>, LlvmError> {
    if matches!(function.kind, FunctionKind::Closure { .. }) {
        let user_params: Vec<IRType> = function.params.iter().map(|p| p.ty.clone()).collect();
        return closure_body_signature(ctx, &user_params, &function.return_type);
    }
    if matches!(
        function.kind,
        FunctionKind::SpawnWrapper { .. } | FunctionKind::ProcessEntryWrapper { .. }
    ) {
        // Spawn / process-entry wrappers are scheduler entry points
        // called through `koja_rt_spawn`'s `void (*)(i8*)` function
        // pointer. The IR signature carries `(config: C) -> Unit` for
        // type-checking convenience, but the LLVM declaration takes
        // the raw config pointer the runtime hands the worker thread;
        // each body emitter loads the typed config out of that
        // pointer in the entry block.
        let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
        return Ok(ctx.context.void_type().fn_type(&[ptr_ty.into()], false));
    }
    let mut param_types: Vec<BasicMetadataTypeEnum<'ctx>> =
        Vec::with_capacity(function.params.len());
    for param in &function.params {
        param_types.push(value_basic_type(ctx, &param.ty)?.into());
    }
    Ok(if matches!(function.return_type, IRType::Unit) {
        ctx.context.void_type().fn_type(&param_types, false)
    } else {
        ir_basic_type(ctx, &function.return_type)?.fn_type(&param_types, false)
    })
}

/// Define a non-entry function's body. Dispatches on
/// [`FunctionKind`]: `Regular` walks the IR basic blocks via
/// [`emit::emit_block`]; `Intrinsic` routes to
/// [`intrinsics::emit_intrinsic_body`] which synthesizes a body from
/// the per-symbol emitter table. Only `main` gets the auto-print
/// wrapper; `Regular` helpers keep the natural `Return`-to-`ret`
/// emission. Pre-creates one inkwell `BasicBlock` per IR block (a
/// no-op for `Intrinsic`'s empty `blocks`) so `Branch` / `CondBranch`
/// terminators can resolve to a real [`BasicBlock`]. The body's
/// [`ValueMap`] is seeded with each [`koja_ir::IRFunctionParam`]
/// bound to the matching `function.get_nth_param(i)` LLVM value
/// before walking the entry block.
pub(crate) fn define_function<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let env_layout = match &function.kind {
        FunctionKind::CloneGlue | FunctionKind::DropGlue => {
            if function.blocks.is_empty() {
                // Collection / `Indirect` glue: a runtime-shaped
                // deep-copy / element-walk synthesized from the operand
                // type at emit time, not from an IR CFG.
                return emit::collection_glue::emit_collection_glue_body(
                    ctx,
                    function,
                    llvm_function,
                );
            }
            // Aggregate glue (struct / enum / union) carries a full
            // `elaborate`-synthesized CFG; emit it exactly like a
            // `Regular` body (no closure env).
            None
        }
        FunctionKind::Intrinsic(id) => {
            return intrinsics::emit_intrinsic_body(ctx, function, llvm_function, id);
        }
        FunctionKind::Extern(_) => {
            // FFI declarations carry no body. Mirrors `Intrinsic`'s
            // skip path but without dispatching to an emitter — the
            // C linker provides the implementation at link time.
            return Ok(());
        }
        FunctionKind::SpawnWrapper { state } => {
            // Spawn wrappers ignore the IR-level placeholder body
            // synthesized by `lower::process` and instead emit the
            // scheduler entry directly: load typed config from the
            // raw `i8*` parameter, call the state's `start`, branch
            // on the `Result` tag, and chain into `run` on success.
            return emit_spawn_wrapper_body(ctx, function, llvm_function, state);
        }
        FunctionKind::ProcessEntryWrapper { state } => {
            // Process-entry wrappers extend the spawn-wrapper shape
            // with an exit-code hand-off: the `StopReason` returned
            // from `run` (or carried in the `Err` arm of `start`)
            // funnels through `ExitStatus.code()` into the module's
            // `__koja_exit_code` global, which the synthesized main
            // trampoline returns from.
            return emit_process_entry_wrapper_body(ctx, function, llvm_function, state);
        }
        FunctionKind::Closure { env_layout } => Some(env_layout.as_slice()),
        FunctionKind::Regular => None,
    };
    // Slot table is per-function — flush any leftovers from the
    // previous helper before walking this body.
    ctx.reset_locals();
    let block_map = declare_blocks(ctx, llvm_function, &function.blocks);
    let mut values = seed_params(function, llvm_function, env_layout.is_some())?;
    let phi_map = emit::declare_block_param_phis(ctx, &function.blocks, &block_map, &mut values)?;
    if let Some(layout) = env_layout {
        let env_ptr = closure_env_ptr(function, llvm_function);
        let env_struct = env_struct_type(ctx, layout)?;
        ctx.set_closure_frame(ClosureFrame {
            env_ptr,
            env_struct,
        });
    }
    ctx.set_block_map(block_map.clone());
    let tco_active = function_has_tail_call(function);
    if tco_active {
        let loop_block = ctx.context.append_basic_block(llvm_function, "tco_loop");
        let param_slots = function
            .params
            .iter()
            .map(|p| (p.local_id, p.ty.clone()))
            .collect();
        ctx.set_tco_frame(TcoFrame {
            loop_block,
            param_slots,
        });
    }
    // Blocks unreachable from the entry block (e.g. the merge of a
    // value-producing `if`/`else` whose arms both diverge) get
    // `unreachable` instead of their natural terminator. The
    // IR layer doesn't model `IRTerminator::Unreachable` yet;
    // the LLVM boundary's reachability walk is the stand-in.
    let reachable = emit::reachable_blocks(&function.blocks);
    let result = (|| -> Result<(), LlvmError> {
        for block in &function.blocks {
            if !reachable.contains(&block.id) {
                emit::emit_unreachable_terminator(ctx, block.id, &block_map)?;
                continue;
            }
            let llvm_block = block_map[&block.id];
            ctx.builder.position_at_end(llvm_block);
            if tco_active && block.id == function.blocks[0].id {
                emit_entry_with_tco_split(ctx, function, block, &block_map, &phi_map, &mut values)?;
            } else {
                emit::emit_block(ctx, block, &block_map, &phi_map, &mut values)?;
            }
        }
        Ok(())
    })();
    if env_layout.is_some() {
        ctx.clear_closure_frame();
    }
    if tco_active {
        ctx.clear_tco_frame();
    }
    ctx.clear_block_map();
    result
}

/// Emit the IR entry block with a tail-call-optimization split.
/// Param promotion (the leading `LocalDecl` + `LocalWrite` per
/// parameter, in order) stays in the LLVM entry block so each
/// function entry runs the param-init exactly once. Then the
/// builder branches to the per-function `tco_loop` header, the
/// rest of the IR entry's instructions emit into that header, and
/// the natural terminator caps it. Subsequent
/// [`IRTerminator::TailCall`] terminators in any block then store
/// fresh args into the matching param slots and branch back to
/// the same `tco_loop` header — a constant-stack iteration.
///
/// Param-promotion length is exactly `2 * function.params.len()`
/// instructions; lower always emits each param's
/// [`IRInstruction::LocalDecl`] + [`IRInstruction::LocalWrite`]
/// pair before any body work. A mismatch would indicate a lower
/// invariant break and panics with a clear message.
fn emit_entry_with_tco_split<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    block: &IRBasicBlock,
    block_map: &BlockMap<'ctx>,
    phi_map: &emit::PhiMap<'ctx>,
    values: &mut ValueMap<'ctx>,
) -> Result<(), LlvmError> {
    let promotion_len = promotion_prefix_len(function);
    if block.instructions.len() < promotion_len {
        panic!(
            "LLVM emit: TCO entry block on `{}` is shorter ({}) than the expected \
             promotion sequence ({}) — lower invariant violation",
            function.symbol,
            block.instructions.len(),
            promotion_len,
        );
    }
    debug_assert_promotion_shape(&block.instructions[..promotion_len], function);
    for instruction in &block.instructions[..promotion_len] {
        emit::emit_instruction_external(ctx, instruction, values)?;
    }
    let frame = ctx.tco_frame().expect("TCO frame must be staged");
    ctx.builder
        .build_unconditional_branch(frame.loop_block)
        .map_err(|e| inkwell_err("TCO entry branch to tco_loop", e))?;
    ctx.builder.position_at_end(frame.loop_block);
    for instruction in &block.instructions[promotion_len..] {
        emit::emit_instruction_external(ctx, instruction, values)?;
    }
    if let Some(insert_block) = ctx.builder.get_insert_block()
        && insert_block.get_terminator().is_some()
    {
        return Ok(());
    }
    emit::emit_terminator_default(ctx, block.id, &block.terminator, values, block_map, phi_map)
}

/// Number of leading entry-block instructions lower emits to promote
/// the parameters into their local slots. Each param is a `LocalDecl`
/// and `LocalWrite` pair (2); a heap-leaf param additionally clones
/// the borrowed argument into its owning slot between the two (3), so
/// the frame holds storage it can drop at scope exit.
fn promotion_prefix_len(function: &IRFunction) -> usize {
    function
        .params
        .iter()
        .map(|param| if is_heap_leaf(&param.ty) { 3 } else { 2 })
        .sum()
}

/// Sanity-check that the entry block's leading instructions are the
/// canonical per-parameter promotion sequences lower emits:
/// `LocalDecl` → (`Clone` for heap-leaf) → `LocalWrite`. A violation
/// indicates a lower invariant break that would otherwise corrupt the
/// back-edge slot writes; we panic with a clear message.
fn debug_assert_promotion_shape(prefix: &[IRInstruction], function: &IRFunction) {
    debug_assert_eq!(prefix.len(), promotion_prefix_len(function));
    let mut cursor = prefix.iter().enumerate();
    for param in &function.params {
        let (index, decl) = cursor.next().expect("promotion prefix shorter than params");
        let IRInstruction::LocalDecl { local, .. } = decl else {
            panic!(
                "LLVM emit: expected `LocalDecl` at promotion #{index} on `{}`, got {decl:?}",
                function.symbol,
            );
        };
        debug_assert_eq!(*local, param.local_id);

        // Heap-leaf params clone the borrowed argument into the slot;
        // the slot then owns `clone.dest` rather than the raw param.
        let stored = if is_heap_leaf(&param.ty) {
            let (index, clone) = cursor
                .next()
                .expect("promotion prefix missing heap-leaf clone");
            let IRInstruction::Clone { dest, source, .. } = clone else {
                panic!(
                    "LLVM emit: expected `Clone` at promotion #{index} on `{}`, got {clone:?}",
                    function.symbol,
                );
            };
            debug_assert_eq!(*source, param.id);
            *dest
        } else {
            param.id
        };

        let (index, write) = cursor.next().expect("promotion prefix missing LocalWrite");
        let IRInstruction::LocalWrite { local, value } = write else {
            panic!(
                "LLVM emit: expected `LocalWrite` at promotion #{index} on `{}`, got {write:?}",
                function.symbol,
            );
        };
        debug_assert_eq!(*local, param.local_id);
        debug_assert_eq!(*value, stored);
    }
}

fn closure_env_ptr<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> inkwell::values::PointerValue<'ctx> {
    llvm_function
        .get_nth_param(0)
        .unwrap_or_else(|| {
            panic!(
                "LLVM emit: closure body `{}` declared no env parameter — \
                 declare_function ABI invariant violation",
                function.symbol,
            )
        })
        .into_pointer_value()
}

/// Pre-create one inkwell [`BasicBlock`] per IR block on
/// `llvm_function`, returning the [`IRBlockId`] -> [`BasicBlock`]
/// index emit consumes when lowering branch terminators. Shared
/// helper used by both the main-wrapper synthesis and helper
/// function definition.
pub(crate) fn declare_blocks<'ctx>(
    ctx: &EmitContext<'ctx>,
    llvm_function: FunctionValue<'ctx>,
    blocks: &[IRBasicBlock],
) -> BlockMap<'ctx> {
    let mut block_map: BTreeMap<IRBlockId, BasicBlock<'ctx>> = BTreeMap::new();
    for block in blocks {
        let llvm_block = ctx.context.append_basic_block(llvm_function, &block.label);
        block_map.insert(block.id, llvm_block);
    }
    block_map
}

/// Seed a fresh [`ValueMap`] with each parameter's LLVM value, keyed
/// by the [`koja_ir::IRFunctionParam::id`] that body lowering
/// uses. Inkwell's `get_nth_param` panics on out-of-bounds; the IR
/// seal guarantees `params.len()` matches the LLVM function's arity,
/// so a miss here is a compiler bug.
///
/// Closure bodies declare an extra env-pointer parameter at LLVM
/// position 0, so user-visible IR params start at LLVM index 1.
/// `is_closure_body` shifts the lookups to keep the LLVM ABI and
/// the IR's `IRFunctionParam` sequence aligned.
fn seed_params<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    is_closure_body: bool,
) -> Result<ValueMap<'ctx>, LlvmError> {
    let llvm_offset: u32 = if is_closure_body { 1 } else { 0 };
    let mut seed = ValueMap::new();
    for (index, param) in function.params.iter().enumerate() {
        let llvm_index = (index as u32) + llvm_offset;
        let llvm_param = llvm_function.get_nth_param(llvm_index).unwrap_or_else(|| {
            panic!(
                "LLVM emit: missing LLVM param #{llvm_index} on `{}` — \
                 signature/IR arity mismatch",
                function.symbol,
            )
        });
        seed.insert(param.id, llvm_param);
    }
    Ok(seed)
}
