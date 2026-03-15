//! Operator compilation: arithmetic, comparison, logical, and unary operators
//! with dispatch based on operand types (integer vs. floating-point).

use expo_ast::ast::{BinOp, Expr, UnaryOp};
use inkwell::values::{BasicValueEnum, FunctionValue};
use inkwell::{FloatPredicate, IntPredicate};

use crate::compiler::Compiler;
use crate::expr::compile_expr;

/// Compiles a binary operation. Dispatches on operand types (float vs int)
/// and supports arithmetic, comparison, and logical operators.
pub fn compile_binary<'ctx>(
    c: &mut Compiler<'ctx>,
    op: &BinOp,
    left: &Expr,
    right: &Expr,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let lhs = compile_expr(c, left, function)?.ok_or("left side of binary op produced no value")?;
    let rhs =
        compile_expr(c, right, function)?.ok_or("right side of binary op produced no value")?;

    if lhs.is_float_value() && rhs.is_float_value() {
        let l = lhs.into_float_value();
        let r = rhs.into_float_value();
        let result = match op {
            BinOp::Add => c.builder.build_float_add(l, r, "fadd").unwrap().into(),
            BinOp::Sub => c.builder.build_float_sub(l, r, "fsub").unwrap().into(),
            BinOp::Mul => c.builder.build_float_mul(l, r, "fmul").unwrap().into(),
            BinOp::Div => c.builder.build_float_div(l, r, "fdiv").unwrap().into(),
            BinOp::Mod => c.builder.build_float_rem(l, r, "frem").unwrap().into(),
            BinOp::Eq => c
                .builder
                .build_float_compare(FloatPredicate::OEQ, l, r, "feq")
                .unwrap()
                .into(),
            BinOp::NotEq => c
                .builder
                .build_float_compare(FloatPredicate::ONE, l, r, "fne")
                .unwrap()
                .into(),
            BinOp::Lt => c
                .builder
                .build_float_compare(FloatPredicate::OLT, l, r, "flt")
                .unwrap()
                .into(),
            BinOp::LtEq => c
                .builder
                .build_float_compare(FloatPredicate::OLE, l, r, "fle")
                .unwrap()
                .into(),
            BinOp::Gt => c
                .builder
                .build_float_compare(FloatPredicate::OGT, l, r, "fgt")
                .unwrap()
                .into(),
            BinOp::GtEq => c
                .builder
                .build_float_compare(FloatPredicate::OGE, l, r, "fge")
                .unwrap()
                .into(),
            _ => return Err(format!("unsupported float binary op: {:?}", op)),
        };
        Ok(Some(result))
    } else if lhs.is_int_value() && rhs.is_int_value() {
        let l = lhs.into_int_value();
        let r = rhs.into_int_value();

        let is_bool = l.get_type().get_bit_width() == 1;

        let result: BasicValueEnum = match op {
            BinOp::Add => c.builder.build_int_add(l, r, "add").unwrap().into(),
            BinOp::Sub => c.builder.build_int_sub(l, r, "sub").unwrap().into(),
            BinOp::Mul => c.builder.build_int_mul(l, r, "mul").unwrap().into(),
            BinOp::Div => c.builder.build_int_signed_div(l, r, "sdiv").unwrap().into(),
            BinOp::Mod => c.builder.build_int_signed_rem(l, r, "srem").unwrap().into(),
            BinOp::Eq => c
                .builder
                .build_int_compare(IntPredicate::EQ, l, r, "eq")
                .unwrap()
                .into(),
            BinOp::NotEq => c
                .builder
                .build_int_compare(IntPredicate::NE, l, r, "ne")
                .unwrap()
                .into(),
            BinOp::Lt => c
                .builder
                .build_int_compare(IntPredicate::SLT, l, r, "slt")
                .unwrap()
                .into(),
            BinOp::LtEq => c
                .builder
                .build_int_compare(IntPredicate::SLE, l, r, "sle")
                .unwrap()
                .into(),
            BinOp::Gt => c
                .builder
                .build_int_compare(IntPredicate::SGT, l, r, "sgt")
                .unwrap()
                .into(),
            BinOp::GtEq => c
                .builder
                .build_int_compare(IntPredicate::SGE, l, r, "sge")
                .unwrap()
                .into(),
            BinOp::And if is_bool => c.builder.build_and(l, r, "and").unwrap().into(),
            BinOp::Or if is_bool => c.builder.build_or(l, r, "or").unwrap().into(),
            BinOp::And | BinOp::Or => {
                return Err("logical operators require bool operands".to_string());
            }
            BinOp::Pipe => return Err("pipe operator not yet supported in compilation".to_string()),
        };
        Ok(Some(result))
    } else {
        Err("mismatched types in binary operation".to_string())
    }
}

/// Compiles a unary operation (negation or logical not).
pub fn compile_unary<'ctx>(
    c: &mut Compiler<'ctx>,
    op: &UnaryOp,
    operand: &Expr,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let val = compile_expr(c, operand, function)?.ok_or("unary operand produced no value")?;

    match op {
        UnaryOp::Neg => {
            if val.is_int_value() {
                Ok(Some(
                    c.builder
                        .build_int_neg(val.into_int_value(), "neg")
                        .unwrap()
                        .into(),
                ))
            } else if val.is_float_value() {
                Ok(Some(
                    c.builder
                        .build_float_neg(val.into_float_value(), "fneg")
                        .unwrap()
                        .into(),
                ))
            } else {
                Err("cannot negate non-numeric value".to_string())
            }
        }
        UnaryOp::Not => {
            if val.is_int_value() {
                Ok(Some(
                    c.builder
                        .build_not(val.into_int_value(), "not")
                        .unwrap()
                        .into(),
                ))
            } else {
                Err("cannot apply 'not' to non-integer value".to_string())
            }
        }
    }
}
