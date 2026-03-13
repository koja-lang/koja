use expo_ast::ast::{BinOp, Expr, Literal, UnaryOp};
use inkwell::values::{BasicValueEnum, FunctionValue};
use inkwell::{FloatPredicate, IntPredicate};

use crate::compiler::Compiler;
use crate::types::to_llvm_type;

pub fn compile_expr<'ctx>(
    c: &mut Compiler<'ctx>,
    expr: &Expr,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    match expr {
        Expr::Literal { value, .. } => compile_literal(c, value),

        Expr::Ident { name, .. } => {
            if let Some((ptr, ty)) = c.variables.get(name) {
                let llvm_ty = to_llvm_type(ty, c.context)
                    .ok_or_else(|| format!("cannot load variable of unsupported type: {name}"))?;
                let val = c.builder.build_load(llvm_ty, *ptr, name).unwrap();
                Ok(Some(val))
            } else {
                Err(format!("undefined variable: {name}"))
            }
        }

        Expr::Group { expr, .. } => compile_expr(c, expr, function),

        Expr::Binary {
            op, left, right, ..
        } => compile_binary(c, op, left, right, function),

        Expr::Unary { op, operand, .. } => compile_unary(c, op, operand, function),

        Expr::Call {
            callee, args, ..
        } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                compile_call(c, name, args, function)
            } else {
                Err("only named function calls are supported".to_string())
            }
        }

        Expr::If {
            condition,
            then_body,
            else_body,
            ..
        } => compile_if(c, condition, then_body, else_body, function),

        _ => Err(format!("not yet supported in compilation: {:?}", std::mem::discriminant(expr))),
    }
}

fn compile_literal<'ctx>(
    c: &Compiler<'ctx>,
    lit: &Literal,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    match lit {
        Literal::Int(s) => {
            let val: i64 = s.parse().map_err(|_| format!("invalid integer: {s}"))?;
            Ok(Some(c.context.i32_type().const_int(val as u64, true).into()))
        }
        Literal::Float(s) => {
            let val: f64 = s.parse().map_err(|_| format!("invalid float: {s}"))?;
            Ok(Some(c.context.f64_type().const_float(val).into()))
        }
        Literal::Bool(b) => Ok(Some(
            c.context
                .bool_type()
                .const_int(if *b { 1 } else { 0 }, false)
                .into(),
        )),
        Literal::Unit => Ok(None),
        Literal::None => Ok(None),
    }
}

fn compile_binary<'ctx>(
    c: &mut Compiler<'ctx>,
    op: &BinOp,
    left: &Expr,
    right: &Expr,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let lhs = compile_expr(c, left, function)?
        .ok_or("left side of binary op produced no value")?;
    let rhs = compile_expr(c, right, function)?
        .ok_or("right side of binary op produced no value")?;

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
                return Err("logical operators require bool operands".to_string())
            }
            BinOp::Pipe => return Err("pipe operator not yet supported in compilation".to_string()),
        };
        Ok(Some(result))
    } else {
        Err("mismatched types in binary operation".to_string())
    }
}

fn compile_unary<'ctx>(
    c: &mut Compiler<'ctx>,
    op: &UnaryOp,
    operand: &Expr,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let val = compile_expr(c, operand, function)?
        .ok_or("unary operand produced no value")?;

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

fn compile_call<'ctx>(
    c: &mut Compiler<'ctx>,
    name: &str,
    args: &[expo_ast::ast::Arg],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    match name {
        "print_i32" | "print_i64" | "print_bool" | "print_f64" => {
            compile_print_builtin(c, name, args, function)
        }
        _ => {
            let callee = *c
                .functions
                .get(name)
                .ok_or_else(|| format!("undefined function: {name}"))?;

            let mut compiled_args = Vec::new();
            for arg in args {
                let val = compile_expr(c, &arg.value, function)?
                    .ok_or_else(|| format!("argument to {name} produced no value"))?;
                compiled_args.push(val.into());
            }

            let result = c
                .builder
                .build_call(callee, &compiled_args, &format!("call_{name}"))
                .unwrap();

            Ok(result.try_as_basic_value().left())
        }
    }
}

fn compile_print_builtin<'ctx>(
    c: &mut Compiler<'ctx>,
    name: &str,
    args: &[expo_ast::ast::Arg],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    if args.len() != 1 {
        return Err(format!("{name} expects exactly 1 argument"));
    }

    let val = compile_expr(c, &args[0].value, function)?
        .ok_or_else(|| format!("argument to {name} produced no value"))?;

    let printf = *c
        .functions
        .get("printf")
        .ok_or("printf not declared")?;

    let fmt_str = match name {
        "print_i32" => "%d\n",
        "print_i64" => "%lld\n",
        "print_f64" => "%f\n",
        "print_bool" => "%d\n",
        _ => return Err(format!("unknown print builtin: {name}")),
    };

    let fmt = c
        .builder
        .build_global_string_ptr(fmt_str, &format!("fmt_{name}"))
        .unwrap();

    c.builder
        .build_call(printf, &[fmt.as_pointer_value().into(), val.into()], "printf_call")
        .unwrap();

    Ok(None)
}

fn compile_if<'ctx>(
    c: &mut Compiler<'ctx>,
    condition: &Expr,
    then_body: &[expo_ast::ast::Statement],
    else_body: &Option<Vec<expo_ast::ast::Statement>>,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let cond_val = compile_expr(c, condition, function)?
        .ok_or("if condition produced no value")?;

    let cond_int = if cond_val.is_int_value() {
        let iv = cond_val.into_int_value();
        if iv.get_type().get_bit_width() == 1 {
            iv
        } else {
            c.builder
                .build_int_compare(
                    IntPredicate::NE,
                    iv,
                    iv.get_type().const_zero(),
                    "ifcond",
                )
                .unwrap()
        }
    } else {
        return Err("if condition must be a boolean".to_string());
    };

    let then_bb = c.context.append_basic_block(function, "then");
    let else_bb = c.context.append_basic_block(function, "else");
    let merge_bb = c.context.append_basic_block(function, "ifcont");

    c.builder
        .build_conditional_branch(cond_int, then_bb, else_bb)
        .unwrap();

    c.builder.position_at_end(then_bb);
    let mut then_val: Option<BasicValueEnum> = None;
    for (i, stmt) in then_body.iter().enumerate() {
        if c.current_block_terminated() {
            break;
        }
        if i == then_body.len() - 1 {
            if let expo_ast::ast::Statement::Expr(expr) = stmt {
                then_val = compile_expr(c, expr, function)?;
                continue;
            }
        }
        crate::stmt::compile_statement(c, stmt, function)?;
    }
    if !c.current_block_terminated() {
        c.builder.build_unconditional_branch(merge_bb).unwrap();
    }
    let then_end_bb = c.builder.get_insert_block().unwrap();

    c.builder.position_at_end(else_bb);
    let mut else_val: Option<BasicValueEnum> = None;
    if let Some(else_stmts) = else_body {
        for (i, stmt) in else_stmts.iter().enumerate() {
            if c.current_block_terminated() {
                break;
            }
            if i == else_stmts.len() - 1 {
                if let expo_ast::ast::Statement::Expr(expr) = stmt {
                    else_val = compile_expr(c, expr, function)?;
                    continue;
                }
            }
            crate::stmt::compile_statement(c, stmt, function)?;
        }
    }
    if !c.current_block_terminated() {
        c.builder.build_unconditional_branch(merge_bb).unwrap();
    }
    let else_end_bb = c.builder.get_insert_block().unwrap();

    c.builder.position_at_end(merge_bb);

    if let (Some(tv), Some(ev)) = (&then_val, &else_val) {
        if tv.get_type() == ev.get_type() {
            let phi = c
                .builder
                .build_phi(tv.get_type(), "ifval")
                .unwrap();
            phi.add_incoming(&[(tv, then_end_bb), (ev, else_end_bb)]);
            return Ok(Some(phi.as_basic_value()));
        }
    }

    Ok(None)
}
