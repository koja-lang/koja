//! Operator compilation: arithmetic, comparison, logical, and unary operators
//! with dispatch based on operand types (integer vs. floating-point).

use expo_ast::ast::{BinOp, Expr, ExprKind, UnaryOp};
use expo_ir::resolved::ops::{
    OperandShape, ResolvedBinaryOp, ResolvedUnaryOp, resolve_binary_op, resolve_unary_op,
};
use expo_typecheck::types::{Primitive, Type};
use inkwell::builder::Builder;
use inkwell::values::{BasicValueEnum, FloatValue, FunctionValue, IntValue};
use inkwell::{FloatPredicate, IntPredicate};

use crate::compiler::{Compiler, ExprResult, TypedValue};
use crate::enums::{compile_enum_struct_eq, enum_mangled_name};
use crate::expr::compile_expr;

/// Compiles a binary operation. Uses [`resolve_binary_op`] to decide what to
/// emit, then mechanically dispatches to the corresponding LLVM builder call.
pub fn compile_binary<'ctx>(
    c: &mut Compiler<'ctx>,
    op: &BinOp,
    left: &Expr,
    right: &Expr,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    if matches!(op, BinOp::Concat) {
        return compile_concat(c, left, right, function);
    }

    let lhs_tv =
        compile_expr(c, left, function)?.ok_or("left side of binary op produced no value")?;
    let rhs_tv =
        compile_expr(c, right, function)?.ok_or("right side of binary op produced no value")?;
    let lhs = lhs_tv.value;
    let rhs = rhs_tv.value;

    let shape = if lhs.is_float_value() && rhs.is_float_value() {
        OperandShape::Float
    } else if lhs.is_int_value() && rhs.is_int_value() {
        OperandShape::Integer {
            bit_width: lhs.into_int_value().get_type().get_bit_width(),
        }
    } else if lhs.is_pointer_value() && rhs.is_pointer_value() {
        OperandShape::Pointer
    } else if lhs.is_struct_value() && rhs.is_struct_value() {
        OperandShape::Struct {
            is_enum: enum_mangled_name(&lhs_tv.expo_type).is_some(),
        }
    } else {
        return Err("mismatched types in binary operation".to_string());
    };

    let resolved = resolve_binary_op(op, &shape)?;

    match resolved {
        ResolvedBinaryOp::BoolAnd => emit_int_arith(c, lhs, rhs, |b, l, r| {
            b.build_and(l, r, "and").unwrap().into()
        }),
        ResolvedBinaryOp::BoolOr => emit_int_arith(c, lhs, rhs, |b, l, r| {
            b.build_or(l, r, "or").unwrap().into()
        }),
        ResolvedBinaryOp::EnumStructEqual { negated } => {
            let eq = compile_enum_struct_eq(c, lhs, rhs, &lhs_tv.expo_type, function)?;
            let result = if negated {
                c.builder.build_not(eq, "enum_ne").unwrap()
            } else {
                eq
            };
            Ok(Some(TypedValue::new(
                result.into(),
                Type::Primitive(Primitive::Bool),
            )))
        }
        ResolvedBinaryOp::FloatAdd => emit_float_arith(c, lhs, rhs, |b, l, r| {
            b.build_float_add(l, r, "fadd").unwrap().into()
        }),
        ResolvedBinaryOp::FloatDiv => emit_float_arith(c, lhs, rhs, |b, l, r| {
            b.build_float_div(l, r, "fdiv").unwrap().into()
        }),
        ResolvedBinaryOp::FloatEqual => emit_float_cmp(c, lhs, rhs, FloatPredicate::OEQ, "feq"),
        ResolvedBinaryOp::FloatGreater => emit_float_cmp(c, lhs, rhs, FloatPredicate::OGT, "fgt"),
        ResolvedBinaryOp::FloatGreaterEqual => {
            emit_float_cmp(c, lhs, rhs, FloatPredicate::OGE, "fge")
        }
        ResolvedBinaryOp::FloatLess => emit_float_cmp(c, lhs, rhs, FloatPredicate::OLT, "flt"),
        ResolvedBinaryOp::FloatLessEqual => emit_float_cmp(c, lhs, rhs, FloatPredicate::OLE, "fle"),
        ResolvedBinaryOp::FloatMul => emit_float_arith(c, lhs, rhs, |b, l, r| {
            b.build_float_mul(l, r, "fmul").unwrap().into()
        }),
        ResolvedBinaryOp::FloatNotEqual => emit_float_cmp(c, lhs, rhs, FloatPredicate::ONE, "fne"),
        ResolvedBinaryOp::FloatRem => emit_float_arith(c, lhs, rhs, |b, l, r| {
            b.build_float_rem(l, r, "frem").unwrap().into()
        }),
        ResolvedBinaryOp::FloatSub => emit_float_arith(c, lhs, rhs, |b, l, r| {
            b.build_float_sub(l, r, "fsub").unwrap().into()
        }),
        ResolvedBinaryOp::IntAdd => emit_int_arith(c, lhs, rhs, |b, l, r| {
            b.build_int_add(l, r, "add").unwrap().into()
        }),
        ResolvedBinaryOp::IntDiv => emit_int_arith(c, lhs, rhs, |b, l, r| {
            b.build_int_signed_div(l, r, "sdiv").unwrap().into()
        }),
        ResolvedBinaryOp::IntEqual => emit_int_cmp(c, lhs, rhs, IntPredicate::EQ, "eq"),
        ResolvedBinaryOp::IntGreater => emit_int_cmp(c, lhs, rhs, IntPredicate::SGT, "sgt"),
        ResolvedBinaryOp::IntGreaterEqual => emit_int_cmp(c, lhs, rhs, IntPredicate::SGE, "sge"),
        ResolvedBinaryOp::IntLess => emit_int_cmp(c, lhs, rhs, IntPredicate::SLT, "slt"),
        ResolvedBinaryOp::IntLessEqual => emit_int_cmp(c, lhs, rhs, IntPredicate::SLE, "sle"),
        ResolvedBinaryOp::IntMul => emit_int_arith(c, lhs, rhs, |b, l, r| {
            b.build_int_mul(l, r, "mul").unwrap().into()
        }),
        ResolvedBinaryOp::IntNotEqual => emit_int_cmp(c, lhs, rhs, IntPredicate::NE, "ne"),
        ResolvedBinaryOp::IntRem => emit_int_arith(c, lhs, rhs, |b, l, r| {
            b.build_int_signed_rem(l, r, "srem").unwrap().into()
        }),
        ResolvedBinaryOp::IntSub => emit_int_arith(c, lhs, rhs, |b, l, r| {
            b.build_int_sub(l, r, "sub").unwrap().into()
        }),
        ResolvedBinaryOp::StringEqual => emit_string_cmp(c, lhs, rhs, IntPredicate::EQ),
        ResolvedBinaryOp::StringNotEqual => emit_string_cmp(c, lhs, rhs, IntPredicate::NE),
    }
}

fn emit_float_arith<'ctx>(
    c: &Compiler<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    build: impl FnOnce(&Builder<'ctx>, FloatValue<'ctx>, FloatValue<'ctx>) -> BasicValueEnum<'ctx>,
) -> ExprResult<'ctx> {
    let result = build(&c.builder, lhs.into_float_value(), rhs.into_float_value());
    Ok(Some(TypedValue::new(
        result,
        Type::Primitive(Primitive::F64),
    )))
}

fn emit_float_cmp<'ctx>(
    c: &Compiler<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    pred: FloatPredicate,
    name: &str,
) -> ExprResult<'ctx> {
    let result = c
        .builder
        .build_float_compare(pred, lhs.into_float_value(), rhs.into_float_value(), name)
        .unwrap();
    Ok(Some(TypedValue::new(
        result.into(),
        Type::Primitive(Primitive::Bool),
    )))
}

fn emit_int_arith<'ctx>(
    c: &Compiler<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    build: impl FnOnce(&Builder<'ctx>, IntValue<'ctx>, IntValue<'ctx>) -> BasicValueEnum<'ctx>,
) -> ExprResult<'ctx> {
    let (l, r) = truncate_to_common_width(c, lhs.into_int_value(), rhs.into_int_value());
    let is_bool = l.get_type().get_bit_width() == 1;
    let result = build(&c.builder, l, r);
    let ty = if is_bool {
        Type::Primitive(Primitive::Bool)
    } else {
        Type::Primitive(Primitive::I64)
    };
    Ok(Some(TypedValue::new(result, ty)))
}

fn emit_int_cmp<'ctx>(
    c: &Compiler<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    pred: IntPredicate,
    name: &str,
) -> ExprResult<'ctx> {
    let (l, r) = truncate_to_common_width(c, lhs.into_int_value(), rhs.into_int_value());
    let result = c.builder.build_int_compare(pred, l, r, name).unwrap();
    Ok(Some(TypedValue::new(
        result.into(),
        Type::Primitive(Primitive::Bool),
    )))
}

fn emit_string_cmp<'ctx>(
    c: &mut Compiler<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    pred: IntPredicate,
) -> ExprResult<'ctx> {
    let strcmp = *c.functions.get("strcmp").ok_or("strcmp not declared")?;
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
    let result = c
        .builder
        .build_int_compare(pred, cmp_result, zero, "str_cmp")
        .unwrap();
    Ok(Some(TypedValue::new(
        result.into(),
        Type::Primitive(Primitive::Bool),
    )))
}

/// Truncates mismatched integer widths to the narrower type. Leaves 1-bit
/// (bool) operands untouched to avoid truncating i64 to i1.
fn truncate_to_common_width<'ctx>(
    c: &Compiler<'ctx>,
    mut l: IntValue<'ctx>,
    mut r: IntValue<'ctx>,
) -> (IntValue<'ctx>, IntValue<'ctx>) {
    let l_bits = l.get_type().get_bit_width();
    let r_bits = r.get_type().get_bit_width();
    if l_bits != r_bits && l_bits > 1 && r_bits > 1 {
        let narrow = l_bits.min(r_bits);
        let target = c.context.custom_width_int_type(narrow);
        if l_bits > narrow {
            l = c.builder.build_int_truncate(l, target, "trunc").unwrap();
        } else {
            r = c.builder.build_int_truncate(r, target, "trunc").unwrap();
        }
    }
    (l, r)
}

enum ConcatKind {
    Binary,
    String,
}

fn resolve_concat_kind(compiler: &Compiler, left: &Expr) -> ConcatKind {
    let operand_type = concat_operand_type(compiler, left);
    match &operand_type {
        Type::Primitive(Primitive::Binary) | Type::Primitive(Primitive::Bits) => ConcatKind::Binary,
        _ => ConcatKind::String,
    }
}

/// Compiles the `<>` concatenation operator for String, Binary, and Bits.
fn compile_concat<'ctx>(
    compiler: &mut Compiler<'ctx>,
    left: &Expr,
    right: &Expr,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let kind = resolve_concat_kind(compiler, left);
    let result_type = concat_operand_type(compiler, left);

    let lhs = compile_expr(compiler, left, function)?
        .ok_or("left side of <> produced no value")?
        .value;
    let rhs = compile_expr(compiler, right, function)?
        .ok_or("right side of <> produced no value")?
        .value;

    let inner = match kind {
        ConcatKind::Binary => compile_binary_concat(compiler, lhs, rhs),
        ConcatKind::String => compile_string_concat(compiler, lhs, rhs),
    }?;
    Ok(inner.map(|v| TypedValue::new(v, result_type)))
}

fn concat_operand_type(compiler: &Compiler, expr: &Expr) -> Type {
    if let ExprKind::Ident { name, .. } = &expr.kind
        && let Some((_, ty, _)) = compiler.fn_state.variables.get(name)
    {
        return ty.clone();
    }
    if matches!(expr.kind, ExprKind::BinaryLiteral { .. }) {
        return Type::Primitive(Primitive::Binary);
    }
    Type::Primitive(Primitive::String)
}

/// String <> String: load bit_lengths from headers, malloc(8 + l_bytes + r_bytes + 1),
/// store combined bit_length, memcpy payloads, null-terminate.
fn compile_string_concat<'ctx>(
    c: &mut Compiler<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let i8_type = c.context.i8_type();
    let i64_type = c.context.i64_type();
    let l_ptr = lhs.into_pointer_value();
    let r_ptr = rhs.into_pointer_value();

    let malloc = *c.functions.get("malloc").ok_or("malloc not declared")?;
    let memcpy = *c.functions.get("memcpy").ok_or("memcpy not declared")?;
    let neg8 = i64_type.const_int((-8i64) as u64, true);
    let eight = i64_type.const_int(8, false);
    let three = i64_type.const_int(3, false);

    let l_hdr_ptr = unsafe {
        c.builder
            .build_gep(i8_type, l_ptr, &[neg8], "l_hdr")
            .unwrap()
    };
    let l_bits = c
        .builder
        .build_load(i64_type, l_hdr_ptr, "l_bits")
        .unwrap()
        .into_int_value();
    let l_bytes = c
        .builder
        .build_right_shift(l_bits, three, false, "l_bytes")
        .unwrap();

    let r_hdr_ptr = unsafe {
        c.builder
            .build_gep(i8_type, r_ptr, &[neg8], "r_hdr")
            .unwrap()
    };
    let r_bits = c
        .builder
        .build_load(i64_type, r_hdr_ptr, "r_bits")
        .unwrap()
        .into_int_value();
    let r_bytes = c
        .builder
        .build_right_shift(r_bits, three, false, "r_bytes")
        .unwrap();

    let total_bits = c
        .builder
        .build_int_add(l_bits, r_bits, "cat_total_bits")
        .unwrap();
    let total_bytes = c
        .builder
        .build_int_add(l_bytes, r_bytes, "cat_total_bytes")
        .unwrap();

    let alloc_size = c
        .builder
        .build_int_add(total_bytes, i64_type.const_int(9, false), "cat_alloc")
        .unwrap();

    let base_ptr = c
        .call(malloc, &[alloc_size.into()], "cat_base")
        .unwrap()
        .into_pointer_value();

    c.builder.build_store(base_ptr, total_bits).unwrap();

    let payload = unsafe {
        c.builder
            .build_in_bounds_gep(i8_type, base_ptr, &[eight], "cat_payload")
            .unwrap()
    };

    c.call_void(
        memcpy,
        &[payload.into(), l_ptr.into(), l_bytes.into()],
        "cat_cpy1",
    );

    let mid = unsafe {
        c.builder
            .build_in_bounds_gep(i8_type, payload, &[l_bytes], "cat_mid")
            .unwrap()
    };
    c.call_void(
        memcpy,
        &[mid.into(), r_ptr.into(), r_bytes.into()],
        "cat_cpy2",
    );

    let end = unsafe {
        c.builder
            .build_in_bounds_gep(i8_type, payload, &[total_bytes], "cat_end")
            .unwrap()
    };
    c.builder
        .build_store(end, i8_type.const_int(0, false))
        .unwrap();

    Ok(Some(payload.into()))
}

/// Binary/Bits <> Binary/Bits: load bit_lengths from headers, compute byte counts,
/// malloc(8 + l_bytes + r_bytes), store combined bit_length, memcpy payloads.
fn compile_binary_concat<'ctx>(
    c: &mut Compiler<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let i8_type = c.context.i8_type();
    let i64_type = c.context.i64_type();
    let l_ptr = lhs.into_pointer_value();
    let r_ptr = rhs.into_pointer_value();

    let malloc = *c.functions.get("malloc").ok_or("malloc not declared")?;
    let memcpy = *c.functions.get("memcpy").ok_or("memcpy not declared")?;

    let neg8 = i64_type.const_int((-8i64) as u64, true);
    let eight = i64_type.const_int(8, false);
    let three = i64_type.const_int(3, false);

    let l_hdr_ptr = unsafe {
        c.builder
            .build_gep(i8_type, l_ptr, &[neg8], "l_hdr_ptr")
            .unwrap()
    };
    let l_bits = c
        .builder
        .build_load(i64_type, l_hdr_ptr, "l_bits")
        .unwrap()
        .into_int_value();
    let l_bytes = c
        .builder
        .build_right_shift(l_bits, three, false, "l_bytes")
        .unwrap();

    let r_hdr_ptr = unsafe {
        c.builder
            .build_gep(i8_type, r_ptr, &[neg8], "r_hdr_ptr")
            .unwrap()
    };
    let r_bits = c
        .builder
        .build_load(i64_type, r_hdr_ptr, "r_bits")
        .unwrap()
        .into_int_value();
    let r_bytes = c
        .builder
        .build_right_shift(r_bits, three, false, "r_bytes")
        .unwrap();

    let total_bits = c
        .builder
        .build_int_add(l_bits, r_bits, "cat_total_bits")
        .unwrap();
    let total_bytes = c
        .builder
        .build_int_add(l_bytes, r_bytes, "cat_total_bytes")
        .unwrap();
    let alloc_size = c
        .builder
        .build_int_add(total_bytes, eight, "cat_alloc")
        .unwrap();

    let base_ptr = c
        .call(malloc, &[alloc_size.into()], "cat_base")
        .unwrap()
        .into_pointer_value();

    c.builder.build_store(base_ptr, total_bits).unwrap();

    let payload = unsafe {
        c.builder
            .build_in_bounds_gep(i8_type, base_ptr, &[eight], "cat_payload")
            .unwrap()
    };

    c.call_void(
        memcpy,
        &[payload.into(), l_ptr.into(), l_bytes.into()],
        "cat_cpy1",
    );

    let mid = unsafe {
        c.builder
            .build_in_bounds_gep(i8_type, payload, &[l_bytes], "cat_mid")
            .unwrap()
    };
    c.call_void(
        memcpy,
        &[mid.into(), r_ptr.into(), r_bytes.into()],
        "cat_cpy2",
    );

    Ok(Some(payload.into()))
}

pub fn compile_unary<'ctx>(
    c: &mut Compiler<'ctx>,
    op: &UnaryOp,
    operand: &Expr,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let tv = compile_expr(c, operand, function)?.ok_or("unary operand produced no value")?;
    let val = tv.value;
    let operand_type = tv.expo_type;

    let shape = if val.is_float_value() {
        OperandShape::Float
    } else if val.is_int_value() {
        OperandShape::Integer {
            bit_width: val.into_int_value().get_type().get_bit_width(),
        }
    } else {
        return Err("unsupported unary operand type".to_string());
    };

    let resolved = resolve_unary_op(op, &shape)?;

    match resolved {
        ResolvedUnaryOp::FloatNeg => Ok(Some(TypedValue::new(
            c.builder
                .build_float_neg(val.into_float_value(), "fneg")
                .unwrap()
                .into(),
            operand_type,
        ))),
        ResolvedUnaryOp::IntNeg => Ok(Some(TypedValue::new(
            c.builder
                .build_int_neg(val.into_int_value(), "neg")
                .unwrap()
                .into(),
            operand_type,
        ))),
        ResolvedUnaryOp::IntNot => Ok(Some(TypedValue::new(
            c.builder
                .build_not(val.into_int_value(), "not")
                .unwrap()
                .into(),
            Type::Primitive(Primitive::Bool),
        ))),
    }
}
