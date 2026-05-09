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

use expo_alpha_ir::{FunctionKind, IRBasicBlock, IRBlockId, IRFunction, IRType};
use inkwell::basic_block::BasicBlock;
use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, FunctionType};
use inkwell::values::FunctionValue;

use crate::ctx::{ClosureFrame, EmitContext};
use crate::emit::{self, BlockMap, ValueMap};
use crate::error::LlvmError;
use crate::intrinsics;
use crate::types::{closure_body_signature, env_struct_type, ir_basic_type};

/// Declare an LLVM function for `function`. The signature mirrors
/// the IR exactly: each [`expo_alpha_ir::IRFunctionParam::ty`]
/// becomes its LLVM basic type and the return type does the same.
/// `Unit` returns / params surface as feature-gap diagnostics
/// through [`ir_basic_type`].
///
/// The LLVM symbol name is picked per-kind:
///
/// - `Regular` / `Intrinsic` declare under
///   [`expo_alpha_ir::IRSymbol::mangled`] (the alpha-internal form).
/// - `Extern(attrs)` declares under
///   [`expo_alpha_ir::IRExternAttrs::link_name`] when present, or
///   the function's bare last segment otherwise (`TestApp.cosf` →
///   `cosf`). The IRSymbol stays the call-site's resolution key,
///   regardless of the LLVM name.
///
/// Idempotent: if `module.get_function(name)` already exists for a
/// previously-seen `link_name` (multiple alpha decls of the same C
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
        FunctionKind::Intrinsic | FunctionKind::Regular => function.symbol.mangled().to_string(),
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
    let mut param_types: Vec<BasicMetadataTypeEnum<'ctx>> =
        Vec::with_capacity(function.params.len());
    for param in &function.params {
        param_types.push(ir_basic_type(ctx, &param.ty)?.into());
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
/// [`ValueMap`] is seeded with each [`expo_alpha_ir::IRFunctionParam`]
/// bound to the matching `function.get_nth_param(i)` LLVM value
/// before walking the entry block.
pub(crate) fn define_function<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    let env_layout = match &function.kind {
        FunctionKind::Intrinsic => {
            return intrinsics::emit_intrinsic_body(ctx, function, llvm_function);
        }
        FunctionKind::Extern(_) => {
            // FFI declarations carry no body. Mirrors `Intrinsic`'s
            // skip path but without dispatching to an emitter — the
            // C linker provides the implementation at link time.
            return Ok(());
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
    // Blocks unreachable from the entry block (e.g. the merge of a
    // value-producing `if`/`else` whose arms both diverge) get
    // `unreachable` instead of their natural terminator. The
    // alpha-IR layer doesn't model `IRTerminator::Unreachable` yet;
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
            emit::emit_block(ctx, block, &block_map, &phi_map, &mut values)?;
        }
        Ok(())
    })();
    if env_layout.is_some() {
        ctx.clear_closure_frame();
    }
    result
}

fn closure_env_ptr<'ctx>(
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> inkwell::values::PointerValue<'ctx> {
    llvm_function
        .get_nth_param(0)
        .unwrap_or_else(|| {
            panic!(
                "alpha LLVM emit: closure body `{}` declared no env parameter — \
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
/// by the [`expo_alpha_ir::IRFunctionParam::id`] that body lowering
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
                "alpha LLVM emit: missing LLVM param #{llvm_index} on `{}` — \
                 signature/IR arity mismatch",
                function.symbol,
            )
        });
        seed.insert(param.id, llvm_param);
    }
    Ok(seed)
}
