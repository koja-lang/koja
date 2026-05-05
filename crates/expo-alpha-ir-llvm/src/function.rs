//! Non-entry function emission: declare an LLVM function for an
//! [`IRFunction`] (no body), then define its body once every helper
//! has been declared. The two-phase declare-then-define pattern lets
//! mutually-recursive calls resolve through `module.get_function`
//! before either body has been walked.
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

use crate::ctx::EmitCtx;
use crate::emit::{self, BlockMap, ValueMap};
use crate::error::LlvmError;
use crate::intrinsics;
use crate::types::ir_basic_type;

/// Declare an LLVM function for `function` under its mangled
/// [`expo_alpha_ir::IRSymbol`]. The signature mirrors the IR exactly:
/// each [`expo_alpha_ir::IRFunctionParam::ty`] becomes its LLVM basic
/// type and the return type does the same. `Unit` returns / params
/// surface as feature-gap diagnostics through [`ir_basic_type`].
pub(crate) fn declare_function<'ctx>(
    ctx: &EmitCtx<'ctx>,
    function: &IRFunction,
) -> Result<FunctionValue<'ctx>, LlvmError> {
    let signature = function_signature(ctx, function)?;
    Ok(ctx.module.add_function(
        function.symbol.mangled(),
        signature,
        Some(Linkage::External),
    ))
}

fn function_signature<'ctx>(
    ctx: &EmitCtx<'ctx>,
    function: &IRFunction,
) -> Result<FunctionType<'ctx>, LlvmError> {
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
    ctx: &EmitCtx<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
) -> Result<(), LlvmError> {
    if function.kind == FunctionKind::Intrinsic {
        return intrinsics::emit_intrinsic_body(ctx, function, llvm_function);
    }
    let block_map = declare_blocks(ctx, llvm_function, &function.blocks);
    let mut values = seed_params(function, llvm_function);
    for block in &function.blocks {
        let llvm_block = block_map[&block.id];
        ctx.builder.position_at_end(llvm_block);
        emit::emit_block(ctx, block, &block_map, &mut values)?;
    }
    Ok(())
}

/// Pre-create one inkwell [`BasicBlock`] per IR block on
/// `llvm_function`, returning the [`IRBlockId`] -> [`BasicBlock`]
/// index emit consumes when lowering branch terminators. Shared
/// helper used by both the main-wrapper synthesis and helper
/// function definition.
pub(crate) fn declare_blocks<'ctx>(
    ctx: &EmitCtx<'ctx>,
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
fn seed_params<'ctx>(function: &IRFunction, llvm_function: FunctionValue<'ctx>) -> ValueMap<'ctx> {
    let mut seed = ValueMap::new();
    for (index, param) in function.params.iter().enumerate() {
        let llvm_param = llvm_function
            .get_nth_param(index as u32)
            .unwrap_or_else(|| {
                panic!(
                    "alpha LLVM emit: missing LLVM param #{index} on `{}` — \
                     signature/IR arity mismatch",
                    function.symbol,
                )
            });
        seed.insert(param.id, llvm_param);
    }
    seed
}
