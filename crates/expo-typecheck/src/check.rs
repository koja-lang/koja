use std::collections::HashMap;

use expo_ast::ast::*;
use expo_ast::span::Span;

use crate::context::TypeContext;
use crate::types::{Primitive, Type, resolve_type_expr};

pub fn check_module(module: &Module, ctx: &mut TypeContext) {
    let struct_names: Vec<String> = ctx.structs.keys().cloned().collect();
    let struct_name_refs: Vec<&str> = struct_names.iter().map(|s| s.as_str()).collect();

    for item in &module.items {
        if let Item::Function(f) = item {
            if !f.type_params.is_empty() {
                continue;
            }
            check_function(f, ctx, &struct_name_refs);
        }
    }
}

fn check_function(f: &Function, ctx: &mut TypeContext, struct_names: &[&str]) {
    let mut env: HashMap<String, Type> = HashMap::new();

    for param in &f.params {
        if let Param::Regular {
            name, type_expr, ..
        } = param
        {
            let ty = resolve_type_expr(type_expr, struct_names);
            env.insert(name.clone(), ty);
        }
    }

    let declared_return = f
        .return_type
        .as_ref()
        .map(|t| resolve_type_expr(t, struct_names))
        .unwrap_or(Type::Unit);

    check_body(&f.body, ctx, &mut env, &declared_return, struct_names);

    if declared_return != Type::Unit && declared_return != Type::Unknown {
        if let Some(last) = f.body.last() {
            if let Statement::Expr(expr) = last {
                let actual = infer_expr(expr, ctx, &mut env, struct_names);
                if actual != Type::Unknown
                    && actual != Type::Error
                    && actual != declared_return
                {
                    ctx.error(
                        format!(
                            "return type mismatch: expected `{}`, found `{}`",
                            declared_return.display(),
                            actual.display()
                        ),
                        expr_span(expr),
                    );
                }
            }
        }
    }
}

fn check_body(
    stmts: &[Statement],
    ctx: &mut TypeContext,
    env: &mut HashMap<String, Type>,
    _return_type: &Type,
    struct_names: &[&str],
) {
    for stmt in stmts {
        check_statement(stmt, ctx, env, _return_type, struct_names);
    }
}

fn check_statement(
    stmt: &Statement,
    ctx: &mut TypeContext,
    env: &mut HashMap<String, Type>,
    return_type: &Type,
    struct_names: &[&str],
) {
    match stmt {
        Statement::Expr(expr) => {
            infer_expr(expr, ctx, env, struct_names);
        }
        Statement::Assignment { target, value, .. } => {
            let value_type = infer_expr(value, ctx, env, struct_names);
            match target {
                AssignTarget::LValue(lv) => {
                    if lv.segments.len() == 1 {
                        let name = &lv.segments[0];
                        if let Some(existing) = env.get(name) {
                            if *existing != Type::Unknown
                                && *existing != Type::Error
                                && value_type != Type::Unknown
                                && value_type != Type::Error
                                && *existing != value_type
                            {
                                ctx.error(
                                    format!(
                                        "type mismatch: `{}` has type `{}`, cannot assign `{}`",
                                        name,
                                        existing.display(),
                                        value_type.display()
                                    ),
                                    lv.span,
                                );
                            }
                        } else {
                            env.insert(name.clone(), value_type);
                        }
                    }
                }
                AssignTarget::Pattern(_) => {}
            }
        }
        Statement::Return { value, span } => {
            let actual = value
                .as_ref()
                .map(|v| infer_expr(v, ctx, env, struct_names))
                .unwrap_or(Type::Unit);
            if *return_type != Type::Unknown
                && *return_type != Type::Error
                && actual != Type::Unknown
                && actual != Type::Error
                && *return_type != actual
            {
                ctx.error(
                    format!(
                        "return type mismatch: expected `{}`, found `{}`",
                        return_type.display(),
                        actual.display()
                    ),
                    *span,
                );
            }
        }
        Statement::CompoundAssign {
            target,
            value,
            span,
            ..
        } => {
            let target_name = &target.segments[0];
            let target_type = env
                .get(target_name)
                .cloned()
                .unwrap_or_else(|| {
                    ctx.error(format!("unknown variable `{}`", target_name), *span);
                    Type::Error
                });
            let value_type = infer_expr(value, ctx, env, struct_names);
            if target_type != Type::Unknown
                && target_type != Type::Error
                && value_type != Type::Unknown
                && value_type != Type::Error
                && !target_type.is_numeric()
            {
                ctx.error(
                    format!(
                        "compound assignment requires numeric type, found `{}`",
                        target_type.display()
                    ),
                    *span,
                );
            }
        }
        Statement::Break { .. } => {}
    }
}

fn infer_expr(
    expr: &Expr,
    ctx: &mut TypeContext,
    env: &mut HashMap<String, Type>,
    struct_names: &[&str],
) -> Type {
    match expr {
        Expr::Literal { value, .. } => match value {
            Literal::Int(_) => Type::Primitive(Primitive::I32),
            Literal::Float(_) => Type::Primitive(Primitive::F64),
            Literal::Bool(_) => Type::Primitive(Primitive::Bool),
            Literal::None => Type::Unknown,
            Literal::Unit => Type::Unit,
        },

        Expr::String { .. } => Type::Primitive(Primitive::String),

        Expr::Ident { name, span } => {
            if let Some(ty) = env.get(name) {
                ty.clone()
            } else if ctx.functions.contains_key(name) {
                Type::Unknown
            } else {
                ctx.error(format!("unknown variable `{}`", name), *span);
                Type::Error
            }
        }

        Expr::Binary {
            op, left, right, span, ..
        } => {
            let left_ty = infer_expr(left, ctx, env, struct_names);
            let right_ty = infer_expr(right, ctx, env, struct_names);

            match op {
                BinOp::And | BinOp::Or => {
                    check_type(&left_ty, &Type::Primitive(Primitive::Bool), *span, ctx);
                    check_type(&right_ty, &Type::Primitive(Primitive::Bool), *span, ctx);
                    Type::Primitive(Primitive::Bool)
                }
                BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                    if left_ty != Type::Unknown
                        && left_ty != Type::Error
                        && right_ty != Type::Unknown
                        && right_ty != Type::Error
                        && left_ty != right_ty
                    {
                        ctx.error(
                            format!(
                                "cannot compare `{}` and `{}`",
                                left_ty.display(),
                                right_ty.display()
                            ),
                            *span,
                        );
                    }
                    Type::Primitive(Primitive::Bool)
                }
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                    if left_ty != Type::Unknown
                        && left_ty != Type::Error
                        && !left_ty.is_numeric()
                    {
                        ctx.error(
                            format!(
                                "arithmetic requires numeric type, found `{}`",
                                left_ty.display()
                            ),
                            *span,
                        );
                        return Type::Error;
                    }
                    if left_ty != Type::Unknown
                        && left_ty != Type::Error
                        && right_ty != Type::Unknown
                        && right_ty != Type::Error
                        && left_ty != right_ty
                    {
                        ctx.error(
                            format!(
                                "type mismatch in arithmetic: `{}` and `{}`",
                                left_ty.display(),
                                right_ty.display()
                            ),
                            *span,
                        );
                        return Type::Error;
                    }
                    if left_ty != Type::Unknown && left_ty != Type::Error {
                        left_ty
                    } else {
                        right_ty
                    }
                }
                BinOp::Pipe => {
                    Type::Unknown
                }
            }
        }

        Expr::Unary { op, operand, span } => {
            let operand_ty = infer_expr(operand, ctx, env, struct_names);
            match op {
                UnaryOp::Neg => {
                    if operand_ty != Type::Unknown
                        && operand_ty != Type::Error
                        && !operand_ty.is_numeric()
                    {
                        ctx.error(
                            format!(
                                "negation requires numeric type, found `{}`",
                                operand_ty.display()
                            ),
                            *span,
                        );
                        Type::Error
                    } else {
                        operand_ty
                    }
                }
                UnaryOp::Not => {
                    check_type(
                        &operand_ty,
                        &Type::Primitive(Primitive::Bool),
                        *span,
                        ctx,
                    );
                    Type::Primitive(Primitive::Bool)
                }
            }
        }

        Expr::Call {
            callee,
            args,
            span,
            ..
        } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                if let Some(sig) = ctx.functions.get(name) {
                    let expected_count = sig.params.len();
                    let actual_count = args.len();
                    let return_type = sig.return_type.clone();
                    let param_types: Vec<(String, Type)> = sig
                        .params
                        .iter()
                        .map(|p| (p.name.clone(), p.ty.clone()))
                        .collect();

                    if expected_count != actual_count {
                        ctx.error(
                            format!(
                                "function `{}` expects {} arguments, got {}",
                                name, expected_count, actual_count
                            ),
                            *span,
                        );
                    } else {
                        for (i, arg) in args.iter().enumerate() {
                            let arg_ty = infer_expr(&arg.value, ctx, env, struct_names);
                            let (param_name, param_ty) = &param_types[i];
                            if *param_ty != Type::Unknown
                                && *param_ty != Type::Error
                                && arg_ty != Type::Unknown
                                && arg_ty != Type::Error
                                && *param_ty != arg_ty
                            {
                                ctx.error(
                                    format!(
                                        "argument `{}`: expected `{}`, found `{}`",
                                        param_name,
                                        param_ty.display(),
                                        arg_ty.display()
                                    ),
                                    arg.span,
                                );
                            }
                        }
                    }
                    return_type
                } else {
                    for arg in args {
                        infer_expr(&arg.value, ctx, env, struct_names);
                    }
                    Type::Unknown
                }
            } else {
                infer_expr(callee, ctx, env, struct_names);
                for arg in args {
                    infer_expr(&arg.value, ctx, env, struct_names);
                }
                Type::Unknown
            }
        }

        Expr::StructConstruction {
            type_path,
            fields,
            span,
        } => {
            let name = type_path.join(".");
            if let Some(struct_info) = ctx.structs.get(&name) {
                let struct_fields = struct_info.fields.clone();
                for fi in fields {
                    let value_ty = infer_expr(&fi.value, ctx, env, struct_names);
                    if let Some((_, field_ty)) =
                        struct_fields.iter().find(|(n, _)| *n == fi.name)
                    {
                        if *field_ty != Type::Unknown
                            && *field_ty != Type::Error
                            && value_ty != Type::Unknown
                            && value_ty != Type::Error
                            && *field_ty != value_ty
                        {
                            ctx.error(
                                format!(
                                    "field `{}`: expected `{}`, found `{}`",
                                    fi.name,
                                    field_ty.display(),
                                    value_ty.display()
                                ),
                                fi.span,
                            );
                        }
                    } else {
                        ctx.error(
                            format!("struct `{}` has no field `{}`", name, fi.name),
                            fi.span,
                        );
                    }
                }
                Type::Struct(name)
            } else {
                for fi in fields {
                    infer_expr(&fi.value, ctx, env, struct_names);
                }
                if struct_names.contains(&name.as_str()) {
                    Type::Struct(name)
                } else {
                    ctx.error(format!("unknown struct `{}`", name), *span);
                    Type::Error
                }
            }
        }

        Expr::FieldAccess {
            receiver,
            field,
            span,
        } => {
            let recv_ty = infer_expr(receiver, ctx, env, struct_names);
            match &recv_ty {
                Type::Struct(name) => {
                    if let Some(struct_info) = ctx.structs.get(name) {
                        if let Some((_, field_ty)) =
                            struct_info.fields.iter().find(|(n, _)| n == field)
                        {
                            field_ty.clone()
                        } else {
                            ctx.error(
                                format!("struct `{}` has no field `{}`", name, field),
                                *span,
                            );
                            Type::Error
                        }
                    } else {
                        Type::Unknown
                    }
                }
                Type::Unknown | Type::Error => recv_ty,
                _ => {
                    ctx.error(
                        format!(
                            "field access on non-struct type `{}`",
                            recv_ty.display()
                        ),
                        *span,
                    );
                    Type::Error
                }
            }
        }

        Expr::If {
            condition,
            then_body,
            else_body,
            ..
        } => {
            let cond_ty = infer_expr(condition, ctx, env, struct_names);
            check_type(
                &cond_ty,
                &Type::Primitive(Primitive::Bool),
                expr_span(expr),
                ctx,
            );
            let mut then_env = env.clone();
            check_body(then_body, ctx, &mut then_env, &Type::Unknown, struct_names);
            if let Some(else_stmts) = else_body {
                let mut else_env = env.clone();
                check_body(else_stmts, ctx, &mut else_env, &Type::Unknown, struct_names);
            }
            Type::Unknown
        }

        Expr::Ternary {
            condition,
            then_expr,
            else_expr,
            span,
        } => {
            let cond_ty = infer_expr(condition, ctx, env, struct_names);
            check_type(&cond_ty, &Type::Primitive(Primitive::Bool), *span, ctx);
            let then_ty = infer_expr(then_expr, ctx, env, struct_names);
            let else_ty = infer_expr(else_expr, ctx, env, struct_names);
            if then_ty != Type::Unknown
                && then_ty != Type::Error
                && else_ty != Type::Unknown
                && else_ty != Type::Error
                && then_ty != else_ty
            {
                ctx.error(
                    format!(
                        "ternary branches have different types: `{}` and `{}`",
                        then_ty.display(),
                        else_ty.display()
                    ),
                    *span,
                );
            }
            if then_ty != Type::Unknown && then_ty != Type::Error {
                then_ty
            } else {
                else_ty
            }
        }

        Expr::Group { expr: inner, .. } => infer_expr(inner, ctx, env, struct_names),

        Expr::Tuple { elements, .. } => {
            let types: Vec<Type> = elements
                .iter()
                .map(|e| infer_expr(e, ctx, env, struct_names))
                .collect();
            Type::Tuple(types)
        }

        Expr::List { elements, .. } => {
            for e in elements {
                infer_expr(e, ctx, env, struct_names);
            }
            Type::Unknown
        }

        Expr::Try { expr: inner, .. } => {
            infer_expr(inner, ctx, env, struct_names);
            Type::Unknown
        }

        Expr::MethodCall { receiver, args, .. } => {
            infer_expr(receiver, ctx, env, struct_names);
            for arg in args {
                infer_expr(&arg.value, ctx, env, struct_names);
            }
            Type::Unknown
        }

        Expr::Spawn { expr: inner, .. } => {
            infer_expr(inner, ctx, env, struct_names);
            Type::Unknown
        }

        Expr::Await { expr: inner, .. } => {
            infer_expr(inner, ctx, env, struct_names);
            Type::Unknown
        }

        Expr::For {
            iterable, body, ..
        } => {
            infer_expr(iterable, ctx, env, struct_names);
            let mut loop_env = env.clone();
            check_body(body, ctx, &mut loop_env, &Type::Unknown, struct_names);
            Type::Unit
        }

        Expr::Loop { body, .. } => {
            let mut loop_env = env.clone();
            check_body(body, ctx, &mut loop_env, &Type::Unknown, struct_names);
            Type::Unknown
        }

        Expr::Unless {
            condition, body, ..
        } => {
            let cond_ty = infer_expr(condition, ctx, env, struct_names);
            check_type(
                &cond_ty,
                &Type::Primitive(Primitive::Bool),
                expr_span(expr),
                ctx,
            );
            let mut branch_env = env.clone();
            check_body(body, ctx, &mut branch_env, &Type::Unknown, struct_names);
            Type::Unknown
        }

        Expr::Match { subject, arms, .. } => {
            infer_expr(subject, ctx, env, struct_names);
            for arm in arms {
                let mut arm_env = env.clone();
                check_body(&arm.body, ctx, &mut arm_env, &Type::Unknown, struct_names);
            }
            Type::Unknown
        }

        Expr::Cond { arms, .. } => {
            for arm in arms {
                infer_expr(&arm.condition, ctx, env, struct_names);
                let mut arm_env = env.clone();
                check_body(&arm.body, ctx, &mut arm_env, &Type::Unknown, struct_names);
            }
            Type::Unknown
        }

        Expr::Closure { body, .. } => {
            let mut closure_env = env.clone();
            check_body(body, ctx, &mut closure_env, &Type::Unknown, struct_names);
            Type::Unknown
        }

        Expr::ShortClosure { body, .. } => {
            infer_expr(body, ctx, env, struct_names);
            Type::Unknown
        }

        _ => Type::Unknown,
    }
}

fn check_type(actual: &Type, expected: &Type, span: Span, ctx: &mut TypeContext) {
    if *actual == Type::Unknown || *actual == Type::Error {
        return;
    }
    if *expected == Type::Unknown || *expected == Type::Error {
        return;
    }
    if actual != expected {
        ctx.error(
            format!(
                "type mismatch: expected `{}`, found `{}`",
                expected.display(),
                actual.display()
            ),
            span,
        );
    }
}

fn expr_span(expr: &Expr) -> Span {
    match expr {
        Expr::Arena { span, .. }
        | Expr::Await { span, .. }
        | Expr::Binary { span, .. }
        | Expr::Call { span, .. }
        | Expr::Closure { span, .. }
        | Expr::Cond { span, .. }
        | Expr::EnumConstruction { span, .. }
        | Expr::FieldAccess { span, .. }
        | Expr::For { span, .. }
        | Expr::Group { span, .. }
        | Expr::Ident { span, .. }
        | Expr::If { span, .. }
        | Expr::List { span, .. }
        | Expr::Literal { span, .. }
        | Expr::Loop { span, .. }
        | Expr::Match { span, .. }
        | Expr::MethodCall { span, .. }
        | Expr::Receive { span, .. }
        | Expr::Self_ { span, .. }
        | Expr::ShortClosure { span, .. }
        | Expr::Spawn { span, .. }
        | Expr::String { span, .. }
        | Expr::StructConstruction { span, .. }
        | Expr::Ternary { span, .. }
        | Expr::Try { span, .. }
        | Expr::Tuple { span, .. }
        | Expr::Unary { span, .. }
        | Expr::Unless { span, .. } => *span,
    }
}
