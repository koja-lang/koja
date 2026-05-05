//! Binary + unary operator emission. Parallel to
//! `expo-alpha-ir-eval/src/ops.rs`: same shape, same arm order, but
//! emitting to LLVM via inkwell instead of stepping in-process.

use expo_alpha_ir::{IRBinOp, IRUnaryOp};
use inkwell::IntPredicate;
use inkwell::values::IntValue;

use crate::ctx::EmitCtx;
use crate::error::LlvmError;

/// Integer arithmetic uses the wrapping `build_int_*` calls (no
/// `nsw`/`nuw`) per Expo's two's-complement overflow contract.
/// Comparisons use signed predicates — the seal pass admits `Int64`
/// only today; threading signedness for `UInt*` is a follow-up.
/// Match-arm order tracks `IRBinOp`'s declaration order in
/// [`expo_alpha_ir`].
pub(super) fn emit_binary_op<'ctx>(
    ctx: &EmitCtx<'ctx>,
    op: IRBinOp,
    lhs: IntValue<'ctx>,
    rhs: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let result = match op {
        IRBinOp::Add => ctx.builder.build_int_add(lhs, rhs, "add"),
        IRBinOp::And => ctx.builder.build_and(lhs, rhs, "and"),
        IRBinOp::Eq => ctx
            .builder
            .build_int_compare(IntPredicate::EQ, lhs, rhs, "eq"),
        IRBinOp::Gt => ctx
            .builder
            .build_int_compare(IntPredicate::SGT, lhs, rhs, "gt"),
        IRBinOp::GtEq => ctx
            .builder
            .build_int_compare(IntPredicate::SGE, lhs, rhs, "gte"),
        IRBinOp::Lt => ctx
            .builder
            .build_int_compare(IntPredicate::SLT, lhs, rhs, "lt"),
        IRBinOp::LtEq => ctx
            .builder
            .build_int_compare(IntPredicate::SLE, lhs, rhs, "lte"),
        IRBinOp::NotEq => ctx
            .builder
            .build_int_compare(IntPredicate::NE, lhs, rhs, "neq"),
        IRBinOp::Or => ctx.builder.build_or(lhs, rhs, "or"),
        IRBinOp::Div | IRBinOp::Mod | IRBinOp::Mul | IRBinOp::Sub => {
            return Err(LlvmError::Codegen(format!(
                "alpha LLVM does not yet emit binary op `{op:?}`",
            )));
        }
    };
    result.map_err(|e| LlvmError::Codegen(format!("inkwell rejected emit for {op:?}: {e}")))
}

/// `Neg` wraps on `i64::MIN` (the eval interpreter's `checked_neg`
/// trap is a known divergence). `Not` is `xor x, -1`; the seal pass
/// only flows `Not` for `Bool`, so `i1` logical-not falls out for
/// free.
pub(super) fn emit_unary_op<'ctx>(
    ctx: &EmitCtx<'ctx>,
    op: IRUnaryOp,
    operand: IntValue<'ctx>,
) -> Result<IntValue<'ctx>, LlvmError> {
    let result = match op {
        IRUnaryOp::Neg => ctx.builder.build_int_neg(operand, "neg"),
        IRUnaryOp::Not => ctx.builder.build_not(operand, "not"),
    };
    result.map_err(|e| LlvmError::Codegen(format!("inkwell rejected emit for {op:?}: {e}")))
}
