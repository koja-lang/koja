//! Non-entry function emission: declare an LLVM function for an
//! [`IRFunction`] (no body), then define its body once every helper
//! has been declared. The two-phase declare-then-define pattern lets
//! mutually-recursive calls resolve through the
//! `IRSymbol -> FunctionValue` index on [`EmitContext`] (populated
//! at declare time) before either body has been walked.
//!
//! The script body (`main`) follows a different shape. See
//! [`crate::main_wrapper::emit_script_main`] for the auto-print
//! scaffolding.

use std::collections::BTreeMap;

use inkwell::AddressSpace;
use inkwell::basic_block::BasicBlock;
use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, FunctionType};
use inkwell::values::FunctionValue;
use koja_ir::{
    FunctionKind, IRBasicBlock, IRBlockId, IRFunction, IRInstruction, IRLocalId, IRSourceDef,
    IRType, ValueId, function_has_tail_call,
};

use crate::ctx::{ClosureFrame, EmitContext, TcoFrame};
use crate::debug::display_name;
use crate::emit::process::{emit_process_entry_wrapper_body, emit_spawn_wrapper_body};
use crate::emit::{self, BlockMap, ValueMap};
use crate::error::{IceExt, LlvmError};
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
///   the function's bare last segment otherwise (`TestApp.cosf` ->
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
        FunctionKind::Closure { .. }
        | FunctionKind::CopyClosureGlue { .. }
        | FunctionKind::DropClosureGlue { .. } => function.symbol.mangled().to_string(),
        FunctionKind::Extern(attrs) => attrs
            .link_name
            .clone()
            .unwrap_or_else(|| function.symbol.last_segment().to_string()),
        FunctionKind::CloneGlue
        | FunctionKind::DeepCopyGlue
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
    if matches!(function.kind, FunctionKind::Extern(_)) {
        // Foreign code can hand back NaN / inf; the call site traps
        // on those to uphold the finite-only `Float` invariant.
        if matches!(function.return_type, IRType::Float32 | IRType::Float64) {
            ctx.register_extern_float_return(function.symbol.clone(), llvm_name.clone());
        }
    } else {
        // FFI declarations carry no body we define. Everything else
        // gets a maintained frame pointer so panic backtraces can
        // walk it.
        ctx.set_frame_pointer(llvm_function);
    }
    ctx.declare_function_debug(
        llvm_function,
        &display_name(&function.symbol),
        debuggable_def_location(function),
    );
    Ok(llvm_function)
}

/// Source location to attribute in DWARF for `function`, or `None`
/// when the function carries no surface-source frame worth showing.
/// Only user-declared `Regular` bodies qualify: synthesized glue,
/// closures, wrappers, and the bodyless `Intrinsic` / `Extern` kinds
/// stay unattributed even when they retain a `def_location`.
fn debuggable_def_location(function: &IRFunction) -> Option<&IRSourceDef> {
    match function.kind {
        FunctionKind::Regular => function.def_location.as_ref(),
        _ => None,
    }
}

fn function_signature<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
) -> Result<FunctionType<'ctx>, LlvmError> {
    if matches!(
        function.kind,
        FunctionKind::Closure { .. } | FunctionKind::DropClosureGlue { .. }
    ) {
        let user_params: Vec<IRType> = function.params.iter().map(|p| p.ty.clone()).collect();
        return closure_body_signature(ctx, &user_params, &function.return_type);
    }
    if matches!(function.kind, FunctionKind::CopyClosureGlue { .. }) {
        // Env deep-copy glue is called by the runtime through the env
        // header's `copy_fn` pointer with an `i8* (i8*)` ABI: env base
        // in, fresh env base out. The IR shell carries no params (the
        // env pointer has no IR type).
        let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
        return Ok(ptr_ty.fn_type(&[ptr_ty.into()], false));
    }
    if matches!(
        function.kind,
        FunctionKind::SpawnWrapper { .. } | FunctionKind::ProcessEntryWrapper { .. }
    ) {
        // Spawn / process-entry wrappers are scheduler entry points
        // called through `koja_rt_spawn`'s `void (*)(i8*)` function
        // pointer. The IR signature carries `(config: C) -> Unit` for
        // type-checking convenience, but the LLVM declaration takes
        // the raw config pointer the runtime hands the worker thread.
        // Each body emitter loads the typed config out of that
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
/// [`emit::emit_block`]. `Intrinsic` routes to
/// [`intrinsics::emit_intrinsic_body`] which synthesizes a body from
/// the per-symbol emitter table. Only `main` gets the auto-print
/// wrapper. `Regular` helpers keep the natural `Return`-to-`ret`
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
    // Stage the body's debug scope before any instruction is built,
    // including the synthetic early-return kinds below, which unset the
    // location so they don't inherit the previous function's scope.
    ctx.enter_function_debug(llvm_function, debuggable_def_location(function));
    let env_layout = match &function.kind {
        FunctionKind::CopyClosureGlue { env_layout } => {
            // Env deep-copy glue has no IR body at all (it returns a
            // raw env pointer). Synthesize the whole `i8* (i8*)` body
            // from the capture layout.
            return emit::closures::emit_copy_closure_glue_body(
                ctx,
                function,
                llvm_function,
                env_layout,
            );
        }
        FunctionKind::CloneGlue | FunctionKind::DeepCopyGlue | FunctionKind::DropGlue => {
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
            // `elaborate`-synthesized CFG. Emit it exactly like a
            // `Regular` body (no closure env).
            None
        }
        FunctionKind::Intrinsic(id) => {
            return intrinsics::emit_intrinsic_body(ctx, function, llvm_function, id);
        }
        FunctionKind::Extern(_) => {
            // FFI declarations carry no body. Mirrors `Intrinsic`'s
            // skip path but without dispatching to an emitter, since
            // the C linker provides the implementation at link time.
            return Ok(());
        }
        FunctionKind::SpawnWrapper { .. } => {
            // Spawn wrappers are pure ABI shims: the real semantics
            // live in the IR-synthesized `<state>.__spawn_body` the
            // wrapper's IR `Call` names. The emitter only loads the
            // typed config from the raw `i8*` parameter and calls it.
            return emit_spawn_wrapper_body(ctx, function, llvm_function);
        }
        FunctionKind::ProcessEntryWrapper { .. } => {
            // Process-entry wrappers extend the spawn-wrapper shim
            // with an exit-code hand-off: the `i64` the IR-synthesized
            // `<state>.__entry_body` returns is stored into the
            // module's `__koja_exit_code` global, which the
            // synthesized main trampoline returns from.
            return emit_process_entry_wrapper_body(ctx, function, llvm_function);
        }
        FunctionKind::Closure { env_layout } | FunctionKind::DropClosureGlue { env_layout } => {
            Some(env_layout.as_slice())
        }
        FunctionKind::Regular => None,
    };
    // Slot table is per-function. Flush any leftovers from the
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
        let param_slots: Vec<(IRLocalId, IRType)> = function
            .params
            .iter()
            .map(|p| (p.local_id, p.ty.clone()))
            .collect();
        let body_slots = preregister_local_slots(ctx, function, &block_map, &param_slots)?;
        ctx.set_tco_frame(TcoFrame {
            body_slots,
            loop_block,
            param_slots,
        });
    }
    // Blocks unreachable from the entry block (e.g. the merge of a
    // value-producing `if`/`else` whose arms both diverge) get
    // `unreachable` instead of their natural terminator. The
    // IR layer doesn't model `IRTerminator::Unreachable` yet.
    // The LLVM boundary's reachability walk is the stand-in.
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

/// Create and register the entry-block `alloca` for every `LocalDecl`
/// in a TCO body before any block is walked, returning the
/// non-parameter `(local, type)` pairs for [`TcoFrame::body_slots`].
///
/// A `TailCall` back-edge zeroes every body slot, and the terminator
/// can be emitted before a later block's `LocalDecl` has run, so the
/// slots must all exist up front. [`crate::emit`]'s `LocalDecl`
/// emitter detects the pre-registered slot and only emits its
/// zero-init store.
fn preregister_local_slots<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    block_map: &BlockMap<'ctx>,
    param_slots: &[(IRLocalId, IRType)],
) -> Result<Vec<(IRLocalId, IRType)>, LlvmError> {
    // `build_entry_alloca` needs an insertion block to find the
    // function. Park the builder at the entry block for the walk.
    ctx.builder
        .position_at_end(block_map[&function.blocks[0].id]);
    let mut body_slots = Vec::new();
    for block in &function.blocks {
        for instruction in &block.instructions {
            let IRInstruction::LocalDecl { local, ty } = instruction else {
                continue;
            };
            let llvm_ty = value_basic_type(ctx, ty)?;
            let slot = ctx.build_entry_alloca(llvm_ty, &local.to_string());
            ctx.register_local_slot(*local, slot);
            if !param_slots.iter().any(|(param, _)| param == local) {
                body_slots.push((*local, ty.clone()));
            }
        }
    }
    Ok(body_slots)
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
/// the same `tco_loop` header, a constant-stack iteration.
///
/// Param-promotion length is exactly `2 * function.params.len()`
/// instructions. Lower always emits each param's
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
    let promotion_len = promotion_prefix_len(function, &block.instructions);
    if block.instructions.len() < promotion_len {
        panic!(
            "LLVM emit: TCO entry block on `{}` is shorter ({}) than the expected \
             promotion sequence ({}), lower invariant violation",
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
        .or_ice()?;
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
/// then `LocalWrite` pair (2). A heap-managed param additionally
/// *acquires* the borrowed argument into its owning slot between the
/// two (3): an inline `Clone` for a heap leaf / no-glue aggregate, or
/// the `Call` the [`koja_ir::elaborate`] pass rewrote a composite
/// clone into. The optional acquire is detected structurally (by its
/// operand referencing the incoming param) so the count tracks
/// whatever lowering and elaborate produced without re-deriving the
/// heap-managed predicate here.
fn promotion_prefix_len(function: &IRFunction, instructions: &[IRInstruction]) -> usize {
    let mut len = 0;
    for param in &function.params {
        len += 1; // LocalDecl
        if is_param_acquire(instructions.get(len), param.id) {
            len += 1; // Clone / rewritten clone-glue Call
        }
        len += 1; // LocalWrite
    }
    len
}

/// Whether `instruction` is the acquire a heap-managed param promotion
/// inserts between its `LocalDecl` and `LocalWrite`: an inline `Clone`
/// of the incoming param, or the `Call` elaborate rewrote that clone
/// into (the param is the call's sole argument).
fn is_param_acquire(instruction: Option<&IRInstruction>, param: ValueId) -> bool {
    match instruction {
        Some(IRInstruction::Clone { source, .. }) => *source == param,
        Some(IRInstruction::Call { args, .. }) => args.first() == Some(&param),
        _ => false,
    }
}

/// Sanity-check that the entry block's leading instructions are the
/// canonical per-parameter promotion sequences lower emits:
/// `LocalDecl` -> (acquire `Clone` / `Call` for heap-managed) ->
/// `LocalWrite`. A violation indicates a lower invariant break that
/// would otherwise corrupt the back-edge slot writes. We panic with a
/// clear message.
fn debug_assert_promotion_shape(prefix: &[IRInstruction], function: &IRFunction) {
    debug_assert_eq!(prefix.len(), promotion_prefix_len(function, prefix));
    let mut cursor = prefix.iter().enumerate().peekable();
    for param in &function.params {
        let (index, decl) = cursor.next().expect("promotion prefix shorter than params");
        let IRInstruction::LocalDecl { local, .. } = decl else {
            panic!(
                "LLVM emit: expected `LocalDecl` at promotion #{index} on `{}`, got {decl:?}",
                function.symbol,
            );
        };
        debug_assert_eq!(*local, param.local_id);

        // A heap-managed param acquires the borrowed argument into its
        // slot, so the slot owns the acquire's `dest` rather than the
        // raw param. The acquire is an inline `Clone` (heap leaf /
        // no-glue aggregate) or the `Call` elaborate rewrote a
        // composite clone into.
        let stored = match cursor.peek() {
            Some((_, IRInstruction::Clone { dest, source, .. })) if *source == param.id => {
                cursor.next();
                *dest
            }
            Some((_, IRInstruction::Call { dest, args, .. }))
                if args.first() == Some(&param.id) =>
            {
                cursor.next();
                *dest
            }
            _ => param.id,
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
                "LLVM emit: closure body `{}` declared no env parameter \
                 (declare_function ABI invariant violation)",
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
/// uses. Inkwell's `get_nth_param` panics on out-of-bounds. The IR
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
                "LLVM emit: missing LLVM param #{llvm_index} on `{}` \
                 (signature/IR arity mismatch)",
                function.symbol,
            )
        });
        seed.insert(param.id, llvm_param);
    }
    Ok(seed)
}
