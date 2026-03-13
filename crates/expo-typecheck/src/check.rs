use std::collections::HashMap;

use expo_ast::ast::*;
use expo_ast::span::Span;

use crate::context::{TypeContext, VariantData};
use crate::types::{Primitive, Type, resolve_type_expr};

pub fn check_module(module: &Module, ctx: &mut TypeContext) {
    let struct_names: Vec<String> = ctx.structs.keys().cloned().collect();
    let struct_name_refs: Vec<&str> = struct_names.iter().map(|s| s.as_str()).collect();
    let enum_names: Vec<String> = ctx.enums.keys().cloned().collect();
    let enum_name_refs: Vec<&str> = enum_names.iter().map(|s| s.as_str()).collect();

    for item in &module.items {
        match item {
            Item::Function(f) => {
                if f.type_params.is_empty() {
                    check_function(f, ctx, None, &struct_name_refs, &enum_name_refs);
                }
            }
            Item::Impl(impl_block) => {
                if impl_block.trait_expr.is_some() {
                    continue;
                }
                let target_name = match &impl_block.target {
                    TypeExpr::Named { path, .. } if path.len() == 1 => &path[0],
                    _ => continue,
                };
                let self_type = if ctx.structs.contains_key(target_name.as_str()) {
                    Type::Struct(target_name.clone())
                } else if ctx.enums.contains_key(target_name.as_str()) {
                    Type::Enum(target_name.clone())
                } else {
                    continue;
                };
                for member in &impl_block.members {
                    if let ImplMember::Function(f) = member {
                        if f.type_params.is_empty() {
                            check_function(
                                f,
                                ctx,
                                Some(&self_type),
                                &struct_name_refs,
                                &enum_name_refs,
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn check_function(
    f: &Function,
    ctx: &mut TypeContext,
    self_type: Option<&Type>,
    struct_names: &[&str],
    enum_names: &[&str],
) {
    let mut env: HashMap<String, Type> = HashMap::new();

    if let Some(ty) = self_type {
        env.insert("self".to_string(), ty.clone());
    }

    for param in &f.params {
        if let Param::Regular {
            name, type_expr, ..
        } = param
        {
            let ty = resolve_type_expr(type_expr, struct_names, enum_names);
            env.insert(name.clone(), ty);
        }
    }

    let declared_return = f
        .return_type
        .as_ref()
        .map(|t| resolve_type_expr(t, struct_names, enum_names))
        .unwrap_or(Type::Unit);

    if f.body.is_empty() {
        return;
    }

    let check_implicit_return = declared_return != Type::Unit && declared_return != Type::Unknown;
    let last_is_expr = matches!(f.body.last(), Some(Statement::Expr(_)));

    // Check all statements except the last if we need to handle implicit return specially
    if check_implicit_return && last_is_expr {
        check_body(
            &f.body[..f.body.len() - 1],
            ctx,
            &mut env,
            &declared_return,
            struct_names,
            enum_names,
        );
        if let Some(Statement::Expr(expr)) = f.body.last() {
            let actual = infer_expr(expr, ctx, &mut env, struct_names, enum_names);
            if actual != Type::Unknown && actual != Type::Error && actual != declared_return {
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
    } else {
        check_body(
            &f.body,
            ctx,
            &mut env,
            &declared_return,
            struct_names,
            enum_names,
        );
    }
}

fn check_body(
    stmts: &[Statement],
    ctx: &mut TypeContext,
    env: &mut HashMap<String, Type>,
    return_type: &Type,
    struct_names: &[&str],
    enum_names: &[&str],
) {
    for stmt in stmts {
        check_statement(stmt, ctx, env, return_type, struct_names, enum_names);
    }
}

fn check_statement(
    stmt: &Statement,
    ctx: &mut TypeContext,
    env: &mut HashMap<String, Type>,
    return_type: &Type,
    struct_names: &[&str],
    enum_names: &[&str],
) {
    match stmt {
        Statement::Assignment { target, value, .. } => {
            let value_type = infer_expr(value, ctx, env, struct_names, enum_names);
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
        Statement::Break { span, .. } => {
            if ctx.loop_depth == 0 {
                ctx.error("break outside of loop".to_string(), *span);
            }
        }
        Statement::CompoundAssign {
            target,
            value,
            span,
            ..
        } => {
            let target_name = &target.segments[0];
            let target_type = env.get(target_name).cloned().unwrap_or_else(|| {
                ctx.error(format!("unknown variable `{}`", target_name), *span);
                Type::Error
            });
            let value_type = infer_expr(value, ctx, env, struct_names, enum_names);
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
        Statement::Expr(expr) => {
            infer_expr(expr, ctx, env, struct_names, enum_names);
        }
        Statement::Return { value, span } => {
            let actual = value
                .as_ref()
                .map(|v| infer_expr(v, ctx, env, struct_names, enum_names))
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
    }
}

fn infer_expr(
    expr: &Expr,
    ctx: &mut TypeContext,
    env: &mut HashMap<String, Type>,
    struct_names: &[&str],
    enum_names: &[&str],
) -> Type {
    match expr {
        Expr::Await { expr: inner, .. } => {
            infer_expr(inner, ctx, env, struct_names, enum_names);
            Type::Unknown
        }

        Expr::Binary {
            op,
            left,
            right,
            span,
            ..
        } => {
            let left_ty = infer_expr(left, ctx, env, struct_names, enum_names);
            let right_ty = infer_expr(right, ctx, env, struct_names, enum_names);

            match op {
                BinOp::Add | BinOp::Div | BinOp::Mod | BinOp::Mul | BinOp::Sub => {
                    if left_ty != Type::Unknown && left_ty != Type::Error && !left_ty.is_numeric() {
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
                BinOp::And | BinOp::Or => {
                    check_type(&left_ty, &Type::Primitive(Primitive::Bool), *span, ctx);
                    check_type(&right_ty, &Type::Primitive(Primitive::Bool), *span, ctx);
                    Type::Primitive(Primitive::Bool)
                }
                BinOp::Eq | BinOp::Gt | BinOp::GtEq | BinOp::Lt | BinOp::LtEq | BinOp::NotEq => {
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
                BinOp::Pipe => Type::Unknown,
            }
        }

        Expr::Call {
            callee, args, span, ..
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
                            let arg_ty = infer_expr(&arg.value, ctx, env, struct_names, enum_names);
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
                        infer_expr(&arg.value, ctx, env, struct_names, enum_names);
                    }
                    Type::Unknown
                }
            } else {
                infer_expr(callee, ctx, env, struct_names, enum_names);
                for arg in args {
                    infer_expr(&arg.value, ctx, env, struct_names, enum_names);
                }
                Type::Unknown
            }
        }

        Expr::Closure { body, .. } => {
            let mut closure_env = env.clone();
            check_body(
                body,
                ctx,
                &mut closure_env,
                &Type::Unknown,
                struct_names,
                enum_names,
            );
            Type::Unknown
        }

        Expr::Cond { arms, .. } => {
            for arm in arms {
                infer_expr(&arm.condition, ctx, env, struct_names, enum_names);
                let mut arm_env = env.clone();
                check_body(
                    &arm.body,
                    ctx,
                    &mut arm_env,
                    &Type::Unknown,
                    struct_names,
                    enum_names,
                );
            }
            Type::Unknown
        }

        Expr::EnumConstruction {
            type_path,
            variant,
            data,
            span,
        } => {
            let enum_name = type_path.join(".");
            if let Some(enum_info) = ctx.enums.get(&enum_name) {
                if let Some(vi) = enum_info.variants.iter().find(|v| v.name == *variant) {
                    let variant_data = vi.data.clone();
                    match (data, &variant_data) {
                        (EnumConstructionData::Unit, VariantData::Unit) => {}
                        (EnumConstructionData::Tuple(args), VariantData::Tuple(expected)) => {
                            if args.len() != expected.len() {
                                ctx.error(
                                    format!(
                                        "variant `{}.{}` expects {} arguments, got {}",
                                        enum_name,
                                        variant,
                                        expected.len(),
                                        args.len()
                                    ),
                                    *span,
                                );
                            } else {
                                for (i, arg_expr) in args.iter().enumerate() {
                                    let arg_ty =
                                        infer_expr(arg_expr, ctx, env, struct_names, enum_names);
                                    let expected_ty = &expected[i];
                                    if *expected_ty != Type::Unknown
                                        && *expected_ty != Type::Error
                                        && arg_ty != Type::Unknown
                                        && arg_ty != Type::Error
                                        && *expected_ty != arg_ty
                                    {
                                        ctx.error(
                                            format!(
                                                "variant `{}.{}` argument {}: expected `{}`, found `{}`",
                                                enum_name, variant, i + 1,
                                                expected_ty.display(),
                                                arg_ty.display()
                                            ),
                                            *span,
                                        );
                                    }
                                }
                            }
                        }
                        (
                            EnumConstructionData::Struct(fields),
                            VariantData::Struct(expected_fields),
                        ) => {
                            for fi in fields {
                                let value_ty =
                                    infer_expr(&fi.value, ctx, env, struct_names, enum_names);
                                if let Some((_, field_ty)) =
                                    expected_fields.iter().find(|(n, _)| *n == fi.name)
                                {
                                    if *field_ty != Type::Unknown
                                        && *field_ty != Type::Error
                                        && value_ty != Type::Unknown
                                        && value_ty != Type::Error
                                        && *field_ty != value_ty
                                    {
                                        ctx.error(
                                            format!(
                                                "variant `{}.{}` field `{}`: expected `{}`, found `{}`",
                                                enum_name, variant, fi.name,
                                                field_ty.display(),
                                                value_ty.display()
                                            ),
                                            fi.span,
                                        );
                                    }
                                } else {
                                    ctx.error(
                                        format!(
                                            "variant `{}.{}` has no field `{}`",
                                            enum_name, variant, fi.name
                                        ),
                                        fi.span,
                                    );
                                }
                            }
                        }
                        (EnumConstructionData::Unit, _) => {
                            ctx.error(
                                format!("variant `{}.{}` requires arguments", enum_name, variant),
                                *span,
                            );
                        }
                        (EnumConstructionData::Tuple(args), _) => {
                            for a in args {
                                infer_expr(a, ctx, env, struct_names, enum_names);
                            }
                            ctx.error(
                                format!(
                                    "variant `{}.{}` does not take positional arguments",
                                    enum_name, variant
                                ),
                                *span,
                            );
                        }
                        (EnumConstructionData::Struct(fields), _) => {
                            for fi in fields {
                                infer_expr(&fi.value, ctx, env, struct_names, enum_names);
                            }
                            ctx.error(
                                format!(
                                    "variant `{}.{}` does not take named fields",
                                    enum_name, variant
                                ),
                                *span,
                            );
                        }
                    }
                    Type::Enum(enum_name)
                } else {
                    ctx.error(
                        format!("enum `{}` has no variant `{}`", enum_name, variant),
                        *span,
                    );
                    Type::Error
                }
            } else {
                match data {
                    EnumConstructionData::Tuple(args) => {
                        for a in args {
                            infer_expr(a, ctx, env, struct_names, enum_names);
                        }
                    }
                    EnumConstructionData::Struct(fields) => {
                        for fi in fields {
                            infer_expr(&fi.value, ctx, env, struct_names, enum_names);
                        }
                    }
                    EnumConstructionData::Unit => {}
                }
                if enum_names.contains(&enum_name.as_str()) {
                    Type::Enum(enum_name)
                } else {
                    Type::Unknown
                }
            }
        }

        Expr::FieldAccess {
            receiver,
            field,
            span,
        } => {
            let recv_ty = infer_expr(receiver, ctx, env, struct_names, enum_names);
            match &recv_ty {
                Type::Struct(name) => {
                    if let Some(struct_info) = ctx.structs.get(name) {
                        if let Some((_, field_ty)) =
                            struct_info.fields.iter().find(|(n, _)| n == field)
                        {
                            field_ty.clone()
                        } else {
                            ctx.error(format!("struct `{}` has no field `{}`", name, field), *span);
                            Type::Error
                        }
                    } else {
                        Type::Unknown
                    }
                }
                Type::Unknown | Type::Error => recv_ty,
                _ => {
                    ctx.error(
                        format!("field access on non-struct type `{}`", recv_ty.display()),
                        *span,
                    );
                    Type::Error
                }
            }
        }

        Expr::For { iterable, body, .. } => {
            infer_expr(iterable, ctx, env, struct_names, enum_names);
            let mut loop_env = env.clone();
            ctx.loop_depth += 1;
            check_body(
                body,
                ctx,
                &mut loop_env,
                &Type::Unknown,
                struct_names,
                enum_names,
            );
            ctx.loop_depth -= 1;
            Type::Unit
        }

        Expr::Group { expr: inner, .. } => infer_expr(inner, ctx, env, struct_names, enum_names),

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

        Expr::If {
            condition,
            then_body,
            else_body,
            ..
        } => {
            let cond_ty = infer_expr(condition, ctx, env, struct_names, enum_names);
            check_type(
                &cond_ty,
                &Type::Primitive(Primitive::Bool),
                expr_span(expr),
                ctx,
            );
            let mut then_env = env.clone();
            check_body(
                then_body,
                ctx,
                &mut then_env,
                &Type::Unknown,
                struct_names,
                enum_names,
            );
            if let Some(else_stmts) = else_body {
                let mut else_env = env.clone();
                check_body(
                    else_stmts,
                    ctx,
                    &mut else_env,
                    &Type::Unknown,
                    struct_names,
                    enum_names,
                );
            }
            Type::Unknown
        }

        Expr::List { elements, .. } => {
            for e in elements {
                infer_expr(e, ctx, env, struct_names, enum_names);
            }
            Type::Unknown
        }

        Expr::Literal { value, .. } => match value {
            Literal::Bool(_) => Type::Primitive(Primitive::Bool),
            Literal::Float(_) => Type::Primitive(Primitive::F64),
            Literal::Int(_) => Type::Primitive(Primitive::I32),
            Literal::None => Type::Unknown,
            Literal::Unit => Type::Unit,
        },

        Expr::Loop { body, .. } => {
            let mut loop_env = env.clone();
            ctx.loop_depth += 1;
            check_body(
                body,
                ctx,
                &mut loop_env,
                &Type::Unknown,
                struct_names,
                enum_names,
            );
            ctx.loop_depth -= 1;
            Type::Unit
        }

        Expr::Match { subject, arms, .. } => {
            let subject_type = infer_expr(subject, ctx, env, struct_names, enum_names);
            for arm in arms {
                let mut arm_env = env.clone();
                check_pattern(&arm.pattern, &subject_type, ctx, &mut arm_env);
                if let Some(guard) = &arm.guard {
                    let guard_ty = infer_expr(guard, ctx, &mut arm_env, struct_names, enum_names);
                    check_type(&guard_ty, &Type::Primitive(Primitive::Bool), arm.span, ctx);
                }
                check_body(
                    &arm.body,
                    ctx,
                    &mut arm_env,
                    &Type::Unknown,
                    struct_names,
                    enum_names,
                );
            }
            Type::Unknown
        }

        Expr::MethodCall {
            receiver,
            method,
            args,
            span,
            ..
        } => {
            let recv_ty = infer_expr(receiver, ctx, env, struct_names, enum_names);

            let method_sig = match &recv_ty {
                Type::Struct(name) => ctx.structs.get(name).and_then(|si| si.methods.get(method)),
                Type::Enum(name) => ctx.enums.get(name).and_then(|ei| ei.methods.get(method)),
                _ => None,
            };

            if let Some(sig) = method_sig {
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
                            "method `{}` expects {} arguments, got {}",
                            method, expected_count, actual_count
                        ),
                        *span,
                    );
                } else {
                    for (i, arg) in args.iter().enumerate() {
                        let arg_ty = infer_expr(&arg.value, ctx, env, struct_names, enum_names);
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
                    infer_expr(&arg.value, ctx, env, struct_names, enum_names);
                }
                match &recv_ty {
                    Type::Struct(name) => {
                        ctx.error(
                            format!("struct `{}` has no method `{}`", name, method),
                            *span,
                        );
                        Type::Error
                    }
                    Type::Enum(name) => {
                        ctx.error(format!("enum `{}` has no method `{}`", name, method), *span);
                        Type::Error
                    }
                    _ => Type::Unknown,
                }
            }
        }

        Expr::ShortClosure { body, .. } => {
            infer_expr(body, ctx, env, struct_names, enum_names);
            Type::Unknown
        }

        Expr::Spawn { expr: inner, .. } => {
            infer_expr(inner, ctx, env, struct_names, enum_names);
            Type::Unknown
        }

        Expr::String { .. } => Type::Primitive(Primitive::String),

        Expr::StructConstruction {
            type_path,
            fields,
            span,
        } => {
            let name = type_path.join(".");
            if let Some(struct_info) = ctx.structs.get(&name) {
                let struct_fields = struct_info.fields.clone();
                for fi in fields {
                    let value_ty = infer_expr(&fi.value, ctx, env, struct_names, enum_names);
                    if let Some((_, field_ty)) = struct_fields.iter().find(|(n, _)| *n == fi.name) {
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
                    infer_expr(&fi.value, ctx, env, struct_names, enum_names);
                }
                if struct_names.contains(&name.as_str()) {
                    Type::Struct(name)
                } else {
                    ctx.error(format!("unknown struct `{}`", name), *span);
                    Type::Error
                }
            }
        }

        Expr::Ternary {
            condition,
            then_expr,
            else_expr,
            span,
        } => {
            let cond_ty = infer_expr(condition, ctx, env, struct_names, enum_names);
            check_type(&cond_ty, &Type::Primitive(Primitive::Bool), *span, ctx);
            let then_ty = infer_expr(then_expr, ctx, env, struct_names, enum_names);
            let else_ty = infer_expr(else_expr, ctx, env, struct_names, enum_names);
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

        Expr::Try { expr: inner, .. } => {
            infer_expr(inner, ctx, env, struct_names, enum_names);
            Type::Unknown
        }

        Expr::Tuple { elements, .. } => {
            let types: Vec<Type> = elements
                .iter()
                .map(|e| infer_expr(e, ctx, env, struct_names, enum_names))
                .collect();
            Type::Tuple(types)
        }

        Expr::Unary { op, operand, span } => {
            let operand_ty = infer_expr(operand, ctx, env, struct_names, enum_names);
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
                    check_type(&operand_ty, &Type::Primitive(Primitive::Bool), *span, ctx);
                    Type::Primitive(Primitive::Bool)
                }
            }
        }

        Expr::Unless {
            condition, body, ..
        } => {
            let cond_ty = infer_expr(condition, ctx, env, struct_names, enum_names);
            check_type(
                &cond_ty,
                &Type::Primitive(Primitive::Bool),
                expr_span(expr),
                ctx,
            );
            let mut branch_env = env.clone();
            check_body(
                body,
                ctx,
                &mut branch_env,
                &Type::Unknown,
                struct_names,
                enum_names,
            );
            Type::Unknown
        }

        Expr::Self_ { span } => {
            if let Some(ty) = env.get("self") {
                ty.clone()
            } else {
                ctx.error("`self` used outside of impl block".to_string(), *span);
                Type::Error
            }
        }

        _ => Type::Unknown,
    }
}

// =========================================================================
// Pattern checking
// =========================================================================

fn check_pattern(
    pat: &Pattern,
    subject_type: &Type,
    ctx: &mut TypeContext,
    env: &mut HashMap<String, Type>,
) {
    match pat {
        Pattern::Binding { name, .. } => {
            env.insert(name.clone(), subject_type.clone());
        }

        Pattern::Constructor {
            name: _, elements, ..
        } => {
            for sub_pat in elements {
                check_pattern(sub_pat, &Type::Unknown, ctx, env);
            }
        }

        Pattern::EnumStruct {
            type_path,
            variant,
            fields,
            span,
        } => {
            let enum_name = type_path.join(".");
            let variant_data = ctx
                .enums
                .get(&enum_name)
                .and_then(|ei| ei.variants.iter().find(|v| v.name == *variant))
                .map(|vi| vi.data.clone());

            match variant_data {
                Some(VariantData::Struct(expected_fields)) => {
                    for fp in fields {
                        if let Some((_, field_ty)) =
                            expected_fields.iter().find(|(n, _)| *n == fp.name)
                        {
                            if let Some(sub_pat) = &fp.pattern {
                                check_pattern(sub_pat, field_ty, ctx, env);
                            } else {
                                env.insert(fp.name.clone(), field_ty.clone());
                            }
                        } else {
                            ctx.error(
                                format!(
                                    "variant `{}.{}` has no field `{}`",
                                    enum_name, variant, fp.name
                                ),
                                fp.span,
                            );
                        }
                    }
                }
                Some(VariantData::Unit) => {
                    ctx.error(
                        format!("variant `{}.{}` has no fields", enum_name, variant),
                        *span,
                    );
                }
                Some(VariantData::Tuple(_)) => {
                    ctx.error(
                        format!(
                            "variant `{}.{}` has positional fields, use ( ) pattern",
                            enum_name, variant
                        ),
                        *span,
                    );
                }
                None => {
                    if ctx.enums.contains_key(&enum_name) {
                        ctx.error(
                            format!("enum `{}` has no variant `{}`", enum_name, variant),
                            *span,
                        );
                    }
                }
            }
        }

        Pattern::EnumTuple {
            type_path,
            variant,
            elements,
            span,
        } => {
            let enum_name = type_path.join(".");
            let variant_data = ctx
                .enums
                .get(&enum_name)
                .and_then(|ei| ei.variants.iter().find(|v| v.name == *variant))
                .map(|vi| vi.data.clone());

            match variant_data {
                Some(VariantData::Tuple(expected_types)) => {
                    if elements.len() != expected_types.len() {
                        ctx.error(
                            format!(
                                "variant `{}.{}` has {} fields, pattern has {}",
                                enum_name,
                                variant,
                                expected_types.len(),
                                elements.len()
                            ),
                            *span,
                        );
                    } else {
                        for (sub_pat, expected_ty) in elements.iter().zip(expected_types.iter()) {
                            check_pattern(sub_pat, expected_ty, ctx, env);
                        }
                    }
                }
                Some(VariantData::Unit) => {
                    ctx.error(
                        format!("variant `{}.{}` has no fields", enum_name, variant),
                        *span,
                    );
                }
                Some(VariantData::Struct(_)) => {
                    ctx.error(
                        format!(
                            "variant `{}.{}` has named fields, use {{ }} pattern",
                            enum_name, variant
                        ),
                        *span,
                    );
                }
                None => {
                    if ctx.enums.contains_key(&enum_name) {
                        ctx.error(
                            format!("enum `{}` has no variant `{}`", enum_name, variant),
                            *span,
                        );
                    }
                }
            }
        }

        Pattern::EnumUnit {
            type_path,
            variant,
            span,
        } => {
            let enum_name = type_path.join(".");
            if let Some(enum_info) = ctx.enums.get(&enum_name) {
                if let Some(vi) = enum_info.variants.iter().find(|v| v.name == *variant) {
                    if !matches!(vi.data, VariantData::Unit) {
                        ctx.error(
                            format!("variant `{}.{}` requires arguments", enum_name, variant),
                            *span,
                        );
                    }
                } else {
                    ctx.error(
                        format!("enum `{}` has no variant `{}`", enum_name, variant),
                        *span,
                    );
                }
            }
        }

        Pattern::List { elements, .. } => {
            for sub_pat in elements {
                check_pattern(sub_pat, &Type::Unknown, ctx, env);
            }
        }

        Pattern::Literal { value, span } => {
            let lit_type = match value {
                Literal::Bool(_) => Type::Primitive(Primitive::Bool),
                Literal::Float(_) => Type::Primitive(Primitive::F64),
                Literal::Int(_) => Type::Primitive(Primitive::I32),
                Literal::None => Type::Unknown,
                Literal::Unit => Type::Unit,
            };
            if lit_type != Type::Unknown
                && *subject_type != Type::Unknown
                && *subject_type != Type::Error
                && lit_type != *subject_type
            {
                ctx.error(
                    format!(
                        "pattern type mismatch: matching `{}` against `{}`",
                        lit_type.display(),
                        subject_type.display()
                    ),
                    *span,
                );
            }
        }

        Pattern::Tuple { elements, span } => match subject_type {
            Type::Tuple(expected_types) => {
                if elements.len() != expected_types.len() {
                    ctx.error(
                        format!(
                            "tuple pattern has {} elements, expected {}",
                            elements.len(),
                            expected_types.len()
                        ),
                        *span,
                    );
                } else {
                    for (sub_pat, expected_ty) in elements.iter().zip(expected_types.iter()) {
                        check_pattern(sub_pat, expected_ty, ctx, env);
                    }
                }
            }
            Type::Unknown | Type::Error => {
                for sub_pat in elements {
                    check_pattern(sub_pat, &Type::Unknown, ctx, env);
                }
            }
            _ => {
                ctx.error(
                    format!(
                        "tuple pattern on non-tuple type `{}`",
                        subject_type.display()
                    ),
                    *span,
                );
            }
        },

        Pattern::Wildcard { .. } => {}
    }
}

// =========================================================================
// Helpers
// =========================================================================

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
