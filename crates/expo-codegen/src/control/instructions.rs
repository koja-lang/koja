//! Shared emission for [`IRInstruction`] sequences within a basic block.
//!
//! Every conditional construct's `emit_*` walker calls
//! [`execute_instructions`] before dispatching the block's terminator.
//! The walker materializes each instruction in order, registers the
//! produced LLVM value under the instruction's SSA destination, and
//! returns the populated `value_map` for the surrounding emitter to
//! pass to [`super::emit_terminator`] when resolving operand
//! references.
//!
//! Instruction emission is mechanical -- all decision logic lives in
//! the resolved op variants ([`ResolvedBinaryOp`], [`ResolvedUnaryOp`])
//! attached at lowering time. Each match arm here maps a resolved
//! variant to the corresponding LLVM builder call(s) with no further
//! choice points.
//!
//! The transitional [`IRInstruction::Stub`] variant defers to
//! `compile_expr`. Its retirement is the long-running migration the
//! IR vocabulary expansion serves: each new typed instruction here
//! retires `Stub` for one [`expo_ast::ast::ExprKind`].

use std::collections::HashMap;

use expo_ir::identity::FunctionIdentifier;
use expo_ir::resolved::ops::{ResolvedBinaryOp, ResolvedUnaryOp};
use expo_ir::values::{IRInstruction, IRValueId};
use inkwell::values::{BasicValueEnum, FunctionValue};
use inkwell::{FloatPredicate, IntPredicate};

use crate::compiler::Compiler;
use crate::expr::compile_expr;
use crate::ops::truncate_to_common_width;

use super::terminator::materialize_operand;

/// Walk `instructions` in order, emitting LLVM IR for each and
/// recording the produced value under the instruction's SSA
/// destination. Returns the populated value map for the caller to
/// thread into [`super::emit_terminator`].
pub(crate) fn execute_instructions<'ctx>(
    compiler: &mut Compiler<'ctx>,
    instructions: &[IRInstruction],
    function: FunctionValue<'ctx>,
) -> Result<HashMap<IRValueId, BasicValueEnum<'ctx>>, String> {
    let mut value_map: HashMap<IRValueId, BasicValueEnum<'ctx>> = HashMap::new();
    for instruction in instructions {
        let (dest, value) = match instruction {
            IRInstruction::BinaryOp { dest, op, lhs, rhs } => {
                let l = materialize_operand(compiler, lhs, &value_map)?;
                let r = materialize_operand(compiler, rhs, &value_map)?;
                let value = emit_binary_op(compiler, op, l, r)?;
                (*dest, value)
            }
            IRInstruction::Stub { dest, expr } => {
                let value = compile_expr(compiler, expr, function)?
                    .ok_or("instruction stub expression produced no value")?
                    .value;
                (*dest, value)
            }
            IRInstruction::UnaryOp { dest, op, operand } => {
                let v = materialize_operand(compiler, operand, &value_map)?;
                let value = emit_unary_op(compiler, op, v)?;
                (*dest, value)
            }
        };
        value_map.insert(dest, value);
    }
    Ok(value_map)
}

/// Map a [`ResolvedBinaryOp`] to its LLVM builder call. Mirrors the
/// per-variant dispatch in `expo-codegen`'s `compile_binary` but
/// operates on already-materialized [`BasicValueEnum`] operands and
/// returns just the produced value (no [`crate::compiler::TypedValue`]
/// wrapping -- the per-block value map carries values only).
fn emit_binary_op<'ctx>(
    c: &mut Compiler<'ctx>,
    op: &ResolvedBinaryOp,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    Ok(match op {
        ResolvedBinaryOp::BoolAnd => {
            let (l, r) = truncate_to_common_width(c, lhs.into_int_value(), rhs.into_int_value());
            c.builder.build_and(l, r, "and").unwrap().into()
        }
        ResolvedBinaryOp::BoolOr => {
            let (l, r) = truncate_to_common_width(c, lhs.into_int_value(), rhs.into_int_value());
            c.builder.build_or(l, r, "or").unwrap().into()
        }
        ResolvedBinaryOp::EnumStructEqual { .. } => {
            return Err(
                "EnumStructEqual is excluded from IRInstruction::BinaryOp; lowering should fall back to Stub"
                    .to_string(),
            );
        }
        ResolvedBinaryOp::FloatAdd => c
            .builder
            .build_float_add(lhs.into_float_value(), rhs.into_float_value(), "fadd")
            .unwrap()
            .into(),
        ResolvedBinaryOp::FloatDiv => c
            .builder
            .build_float_div(lhs.into_float_value(), rhs.into_float_value(), "fdiv")
            .unwrap()
            .into(),
        ResolvedBinaryOp::FloatEqual => emit_float_cmp(c, lhs, rhs, FloatPredicate::OEQ, "feq"),
        ResolvedBinaryOp::FloatGreater => emit_float_cmp(c, lhs, rhs, FloatPredicate::OGT, "fgt"),
        ResolvedBinaryOp::FloatGreaterEqual => {
            emit_float_cmp(c, lhs, rhs, FloatPredicate::OGE, "fge")
        }
        ResolvedBinaryOp::FloatLess => emit_float_cmp(c, lhs, rhs, FloatPredicate::OLT, "flt"),
        ResolvedBinaryOp::FloatLessEqual => emit_float_cmp(c, lhs, rhs, FloatPredicate::OLE, "fle"),
        ResolvedBinaryOp::FloatMul => c
            .builder
            .build_float_mul(lhs.into_float_value(), rhs.into_float_value(), "fmul")
            .unwrap()
            .into(),
        ResolvedBinaryOp::FloatNotEqual => emit_float_cmp(c, lhs, rhs, FloatPredicate::ONE, "fne"),
        ResolvedBinaryOp::FloatRem => c
            .builder
            .build_float_rem(lhs.into_float_value(), rhs.into_float_value(), "frem")
            .unwrap()
            .into(),
        ResolvedBinaryOp::FloatSub => c
            .builder
            .build_float_sub(lhs.into_float_value(), rhs.into_float_value(), "fsub")
            .unwrap()
            .into(),
        ResolvedBinaryOp::IntAdd => emit_int_arith_simple(c, lhs, rhs, |b, l, r| {
            b.build_int_add(l, r, "add").unwrap().into()
        }),
        ResolvedBinaryOp::IntDiv => emit_int_arith_simple(c, lhs, rhs, |b, l, r| {
            b.build_int_signed_div(l, r, "sdiv").unwrap().into()
        }),
        ResolvedBinaryOp::IntEqual => emit_int_cmp(c, lhs, rhs, IntPredicate::EQ, "eq"),
        ResolvedBinaryOp::IntGreater => emit_int_cmp(c, lhs, rhs, IntPredicate::SGT, "sgt"),
        ResolvedBinaryOp::IntGreaterEqual => emit_int_cmp(c, lhs, rhs, IntPredicate::SGE, "sge"),
        ResolvedBinaryOp::IntLess => emit_int_cmp(c, lhs, rhs, IntPredicate::SLT, "slt"),
        ResolvedBinaryOp::IntLessEqual => emit_int_cmp(c, lhs, rhs, IntPredicate::SLE, "sle"),
        ResolvedBinaryOp::IntMul => emit_int_arith_simple(c, lhs, rhs, |b, l, r| {
            b.build_int_mul(l, r, "mul").unwrap().into()
        }),
        ResolvedBinaryOp::IntNotEqual => emit_int_cmp(c, lhs, rhs, IntPredicate::NE, "ne"),
        ResolvedBinaryOp::IntRem => emit_int_arith_simple(c, lhs, rhs, |b, l, r| {
            b.build_int_signed_rem(l, r, "srem").unwrap().into()
        }),
        ResolvedBinaryOp::IntSub => emit_int_arith_simple(c, lhs, rhs, |b, l, r| {
            b.build_int_sub(l, r, "sub").unwrap().into()
        }),
        ResolvedBinaryOp::StringEqual => emit_string_cmp(c, lhs, rhs, IntPredicate::EQ)?,
        ResolvedBinaryOp::StringNotEqual => emit_string_cmp(c, lhs, rhs, IntPredicate::NE)?,
    })
}

fn emit_unary_op<'ctx>(
    c: &mut Compiler<'ctx>,
    op: &ResolvedUnaryOp,
    operand: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    Ok(match op {
        ResolvedUnaryOp::FloatNeg => c
            .builder
            .build_float_neg(operand.into_float_value(), "fneg")
            .unwrap()
            .into(),
        ResolvedUnaryOp::IntNeg => c
            .builder
            .build_int_neg(operand.into_int_value(), "neg")
            .unwrap()
            .into(),
        ResolvedUnaryOp::IntNot => c
            .builder
            .build_not(operand.into_int_value(), "not")
            .unwrap()
            .into(),
    })
}

fn emit_int_arith_simple<'ctx>(
    c: &mut Compiler<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    build: impl FnOnce(
        &inkwell::builder::Builder<'ctx>,
        inkwell::values::IntValue<'ctx>,
        inkwell::values::IntValue<'ctx>,
    ) -> BasicValueEnum<'ctx>,
) -> BasicValueEnum<'ctx> {
    let (l, r) = truncate_to_common_width(c, lhs.into_int_value(), rhs.into_int_value());
    build(&c.builder, l, r)
}

fn emit_int_cmp<'ctx>(
    c: &mut Compiler<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    pred: IntPredicate,
    name: &str,
) -> BasicValueEnum<'ctx> {
    let (l, r) = truncate_to_common_width(c, lhs.into_int_value(), rhs.into_int_value());
    c.builder
        .build_int_compare(pred, l, r, name)
        .unwrap()
        .into()
}

fn emit_float_cmp<'ctx>(
    c: &Compiler<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    pred: FloatPredicate,
    name: &str,
) -> BasicValueEnum<'ctx> {
    c.builder
        .build_float_compare(pred, lhs.into_float_value(), rhs.into_float_value(), name)
        .unwrap()
        .into()
}

fn emit_string_cmp<'ctx>(
    c: &mut Compiler<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    pred: IntPredicate,
) -> Result<BasicValueEnum<'ctx>, String> {
    let strcmp = *c
        .functions
        .get(&FunctionIdentifier::new("strcmp"))
        .ok_or("strcmp not declared")?;
    let cmp_result = c
        .call(
            strcmp,
            &[
                lhs.into_pointer_value().into(),
                rhs.into_pointer_value().into(),
            ],
            "strcmp_result",
        )
        .ok_or("strcmp did not return a value")?
        .into_int_value();
    let zero = c.context.i32_type().const_int(0, false);
    Ok(c.builder
        .build_int_compare(pred, cmp_result, zero, "str_cmp")
        .unwrap()
        .into())
}
