//! Operator compilation: arithmetic, comparison, logical, and unary operators
//! with dispatch based on operand types (integer vs. floating-point).

use expo_ast::ast::{BinOp, Expr, UnaryOp};
use expo_typecheck::types::{Primitive, Type};
use inkwell::values::{BasicValueEnum, FunctionValue};
use inkwell::{FloatPredicate, IntPredicate};

use crate::compiler::{Compiler, ExprResult, TypedValue};
use crate::enums::{compile_enum_struct_eq, enum_mangled_name};
use crate::expr::compile_expr;

/// Compiles a binary operation. Dispatches on operand types (float vs int)
/// and supports arithmetic, comparison, and logical operators.
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

    let is_comparison = matches!(
        op,
        BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq
    );

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
        let ty = if is_comparison {
            Type::Primitive(Primitive::Bool)
        } else {
            Type::Primitive(Primitive::F64)
        };
        Ok(Some(TypedValue::new(result, ty)))
    } else if lhs.is_int_value() && rhs.is_int_value() {
        let mut l = lhs.into_int_value();
        let mut r = rhs.into_int_value();

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
            BinOp::Concat => unreachable!("handled by early return"),
        };
        let ty = if is_comparison || is_bool {
            Type::Primitive(Primitive::Bool)
        } else {
            Type::Primitive(Primitive::I64)
        };
        Ok(Some(TypedValue::new(result, ty)))
    } else if lhs.is_pointer_value() && rhs.is_pointer_value() {
        let l = lhs.into_pointer_value();
        let r = rhs.into_pointer_value();
        match op {
            BinOp::Eq | BinOp::NotEq => {
                let strcmp = *c.functions.get("strcmp").ok_or("strcmp not declared")?;
                let cmp_result = c
                    .call(strcmp, &[l.into(), r.into()], "strcmp_result")
                    .ok_or("strcmp did not return a value")?
                    .into_int_value();
                let zero = c.context.i32_type().const_int(0, false);
                let pred = if matches!(op, BinOp::Eq) {
                    IntPredicate::EQ
                } else {
                    IntPredicate::NE
                };
                let result = c
                    .builder
                    .build_int_compare(pred, cmp_result, zero, "str_cmp")
                    .unwrap();
                Ok(Some(TypedValue::new(
                    result.into(),
                    Type::Primitive(Primitive::Bool),
                )))
            }
            _ => Err(format!("unsupported string binary op: {:?}", op)),
        }
    } else if lhs.is_struct_value()
        && rhs.is_struct_value()
        && matches!(op, BinOp::Eq | BinOp::NotEq)
        && enum_mangled_name(&lhs_tv.expo_type).is_some()
    {
        let eq = compile_enum_struct_eq(c, lhs, rhs, &lhs_tv.expo_type, function)?;
        let result = if matches!(op, BinOp::NotEq) {
            c.builder.build_not(eq, "enum_ne").unwrap()
        } else {
            eq
        };
        Ok(Some(TypedValue::new(
            result.into(),
            Type::Primitive(Primitive::Bool),
        )))
    } else {
        Err("mismatched types in binary operation".to_string())
    }
}

/// Compiles the `<>` concatenation operator for String, Binary, and Bits.
fn compile_concat<'ctx>(
    c: &mut Compiler<'ctx>,
    left: &Expr,
    right: &Expr,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let lhs_ty = concat_operand_type(c, left);
    let lhs = compile_expr(c, left, function)?
        .ok_or("left side of <> produced no value")?
        .value;
    let rhs = compile_expr(c, right, function)?
        .ok_or("right side of <> produced no value")?
        .value;

    let result_ty = lhs_ty.clone();
    let inner = match &lhs_ty {
        Type::Primitive(Primitive::Binary) | Type::Primitive(Primitive::Bits) => {
            compile_binary_concat(c, lhs, rhs)
        }
        _ => compile_string_concat(c, lhs, rhs),
    }?;
    Ok(inner.map(|v| TypedValue::new(v, result_ty)))
}

fn concat_operand_type(c: &Compiler, expr: &Expr) -> Type {
    if let Expr::Ident { name, .. } = expr
        && let Some((_, ty, _)) = c.fn_state.variables.get(name)
    {
        return ty.clone();
    }
    if matches!(expr, Expr::BinaryLiteral { .. }) {
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

/// Compiles a unary operation (negation or logical not).
pub fn compile_unary<'ctx>(
    c: &mut Compiler<'ctx>,
    op: &UnaryOp,
    operand: &Expr,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let tv = compile_expr(c, operand, function)?.ok_or("unary operand produced no value")?;
    let val = tv.value;
    let operand_type = tv.expo_type;

    match op {
        UnaryOp::Neg => {
            if val.is_int_value() {
                Ok(Some(TypedValue::new(
                    c.builder
                        .build_int_neg(val.into_int_value(), "neg")
                        .unwrap()
                        .into(),
                    operand_type,
                )))
            } else if val.is_float_value() {
                Ok(Some(TypedValue::new(
                    c.builder
                        .build_float_neg(val.into_float_value(), "fneg")
                        .unwrap()
                        .into(),
                    operand_type,
                )))
            } else {
                Err("cannot negate non-numeric value".to_string())
            }
        }
        UnaryOp::Not => {
            if val.is_int_value() {
                Ok(Some(TypedValue::new(
                    c.builder
                        .build_not(val.into_int_value(), "not")
                        .unwrap()
                        .into(),
                    Type::Primitive(Primitive::Bool),
                )))
            } else {
                Err("cannot apply 'not' to non-integer value".to_string())
            }
        }
    }
}
