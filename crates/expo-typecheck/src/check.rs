use std::collections::{HashMap, HashSet};

use expo_ast::ast::*;
use expo_ast::span::Span;

use crate::context::{FunctionSig, ParamInfo, TypeContext, VariantData};
use crate::types::{
    GenericKind, Primitive, Type, build_substitution, resolve_type_expr, substitute, unify,
};

/// Per-function environment used during type checking, tracking local variable
/// types, the expected return type, and loop nesting depth.
struct CheckEnv<'a> {
    env: HashMap<String, Type>,
    used_vars: HashSet<String>,
    loop_depth: usize,
    return_type: Type,
    struct_names: &'a [&'a str],
    enum_names: &'a [&'a str],
}

impl<'a> CheckEnv<'a> {
    fn child(&self, return_type: Type) -> CheckEnv<'a> {
        CheckEnv {
            env: self.env.clone(),
            used_vars: HashSet::new(),
            loop_depth: self.loop_depth,
            return_type,
            struct_names: self.struct_names,
            enum_names: self.enum_names,
        }
    }
}

/// Type-checks all function bodies and impl blocks in a module, emitting
/// diagnostics for type mismatches, undefined variables, and exhaustiveness errors.
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
                let (target_name, is_generic_impl) = match &impl_block.target {
                    TypeExpr::Named { path, .. } if path.len() == 1 => (&path[0], false),
                    TypeExpr::Generic { path, .. } if path.len() == 1 => (&path[0], true),
                    _ => continue,
                };
                if is_generic_impl {
                    continue;
                }
                let self_type = if ctx.structs.contains_key(target_name.as_str()) {
                    Type::Struct(target_name.clone())
                } else if ctx.enums.contains_key(target_name.as_str()) {
                    Type::Enum(target_name.clone())
                } else {
                    continue;
                };
                for member in &impl_block.members {
                    if let ImplMember::Function(f) = member
                        && f.type_params.is_empty()
                    {
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

    let mut ce = CheckEnv {
        env,
        used_vars: HashSet::new(),
        loop_depth: 0,
        return_type: declared_return.clone(),
        struct_names,
        enum_names,
    };

    let check_implicit_return = declared_return != Type::Unit && declared_return != Type::Unknown;
    let last_is_expr = matches!(f.body.last(), Some(Statement::Expr(_)));

    if check_implicit_return && last_is_expr {
        check_body(&f.body[..f.body.len() - 1], ctx, &mut ce);
        if let Some(Statement::Expr(expr)) = f.body.last() {
            let actual = infer_expr(expr, ctx, &mut ce);
            if actual.is_known() && actual != declared_return {
                ctx.error_with_hint(
                    format!(
                        "return type mismatch: expected `{}`, found `{}`",
                        declared_return.display(),
                        actual.display()
                    ),
                    format!(
                        "function is declared to return `{}`",
                        declared_return.display()
                    ),
                    expr_span(expr),
                );
            }
        }
    } else {
        check_body(&f.body, ctx, &mut ce);
    }
}

fn check_body(stmts: &[Statement], ctx: &mut TypeContext, ce: &mut CheckEnv) {
    for stmt in stmts {
        check_statement(stmt, ctx, ce);
    }
}

fn check_statement(stmt: &Statement, ctx: &mut TypeContext, ce: &mut CheckEnv) {
    match stmt {
        Statement::Assignment {
            target,
            type_annotation,
            value,
            span,
        } => {
            let value_type = infer_expr(value, ctx, ce);

            let effective_type = if let Some(te) = type_annotation {
                let annotated = resolve_type_expr(te, ce.struct_names, ce.enum_names);
                if value_type.is_known()
                    && annotated.is_known()
                    && !types_compatible(&value_type, &annotated)
                {
                    ctx.error_with_hint(
                        format!(
                            "type mismatch: annotation is `{}` but value has type `{}`",
                            annotated.display(),
                            value_type.display()
                        ),
                        "ensure the annotation matches the expression type".into(),
                        *span,
                    );
                }
                annotated
            } else {
                value_type
            };

            match target {
                AssignTarget::LValue(lv) => {
                    if lv.segments.len() == 1 {
                        let name = &lv.segments[0];
                        if ctx.constants.contains_key(name) {
                            ctx.error_with_hint(
                                format!("cannot assign to constant `{}`", name),
                                "constants are immutable and cannot be reassigned".into(),
                                lv.span,
                            );
                            return;
                        }
                        if let Some(existing) = ce.env.get(name) {
                            if existing.is_known()
                                && effective_type.is_known()
                                && !types_compatible(existing, &effective_type)
                            {
                                ctx.error_with_hint(
                                    format!(
                                        "type mismatch: `{}` has type `{}`, cannot assign `{}`",
                                        name,
                                        existing.display(),
                                        effective_type.display()
                                    ),
                                    format!(
                                        "variable was first assigned as `{}`",
                                        existing.display()
                                    ),
                                    lv.span,
                                );
                            }
                        } else {
                            ce.env.insert(name.clone(), effective_type);
                        }
                    }
                }
                AssignTarget::Pattern(_) => {}
            }
        }
        Statement::Break { span, .. } => {
            if ce.loop_depth == 0 {
                ctx.error_with_hint(
                    "break outside of loop".to_string(),
                    "'break' can only be used inside 'loop' or 'while'".into(),
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
            if ctx.constants.contains_key(target_name) {
                ctx.error_with_hint(
                    format!("cannot assign to constant `{}`", target_name),
                    "constants are immutable and cannot be reassigned".into(),
                    *span,
                );
                return;
            }
            let target_type = ce.env.get(target_name).cloned().unwrap_or_else(|| {
                ctx.error_with_hint(
                    format!("unknown variable `{}`", target_name),
                    "check the spelling or make sure it is defined before this line".into(),
                    *span,
                );
                Type::Error
            });
            let value_type = infer_expr(value, ctx, ce);
            if target_type.is_known() && value_type.is_known() && !target_type.is_numeric() {
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
            infer_expr(expr, ctx, ce);
        }
        Statement::Return { value, span } => {
            let actual = value
                .as_ref()
                .map(|v| infer_expr(v, ctx, ce))
                .unwrap_or(Type::Unit);
            if ce.return_type.is_known() && actual.is_known() && ce.return_type != actual {
                ctx.error_with_hint(
                    format!(
                        "return type mismatch: expected `{}`, found `{}`",
                        ce.return_type.display(),
                        actual.display()
                    ),
                    format!(
                        "function is declared to return `{}`",
                        ce.return_type.display()
                    ),
                    *span,
                );
            }
        }
    }
}

/// Infers the type of an expression, emitting diagnostics for any type errors
/// encountered during traversal. Returns `Type::Unknown` when the type cannot
/// be determined.
fn infer_expr(expr: &Expr, ctx: &mut TypeContext, ce: &mut CheckEnv) -> Type {
    match expr {
        Expr::Await { expr: inner, .. } => {
            infer_expr(inner, ctx, ce);
            Type::Unknown
        }

        Expr::Binary {
            op,
            left,
            right,
            span,
            ..
        } => infer_binary(op, left, right, *span, ctx, ce),

        Expr::Call {
            callee, args, span, ..
        } => infer_call(callee, args, *span, ctx, ce),

        Expr::Closure {
            params, body, span, ..
        } => {
            let mut closure_env = CheckEnv {
                env: HashMap::new(),
                used_vars: HashSet::new(),
                loop_depth: 0,
                return_type: Type::Unknown,
                struct_names: ce.struct_names,
                enum_names: ce.enum_names,
            };
            let param_types = bind_closure_params(params, &mut closure_env, ctx, *span);
            check_body(body, ctx, &mut closure_env);
            let return_type = body
                .last()
                .and_then(|s| match s {
                    Statement::Expr(e) => Some(infer_expr(e, ctx, &mut closure_env)),
                    _ => None,
                })
                .unwrap_or(Type::Unit);
            Type::Function {
                params: param_types,
                return_type: Box::new(return_type),
            }
        }

        Expr::Cond {
            arms, else_body, ..
        } => {
            for arm in arms {
                infer_expr(&arm.condition, ctx, ce);
                let mut child = ce.child(Type::Unknown);
                check_body(&arm.body, ctx, &mut child);
            }
            if let Some(body) = else_body {
                let mut child = ce.child(Type::Unknown);
                check_body(body, ctx, &mut child);
            }
            Type::Unknown
        }

        Expr::EnumConstruction {
            type_path,
            variant,
            data,
            span,
        } => infer_enum_construction(type_path, variant, data, *span, ctx, ce),

        Expr::FieldAccess {
            receiver,
            field,
            span,
        } => infer_field_access(receiver, field, *span, ctx, ce),

        Expr::For { iterable, body, .. } => {
            infer_expr(iterable, ctx, ce);
            let mut child = ce.child(Type::Unknown);
            child.loop_depth += 1;
            check_body(body, ctx, &mut child);
            Type::Unit
        }

        Expr::Group { expr: inner, .. } => infer_expr(inner, ctx, ce),

        Expr::Ident { name, span } => {
            if let Some(ty) = ce.env.get(name) {
                ce.used_vars.insert(name.clone());
                ty.clone()
            } else if let Some(ty) = ctx.constants.get(name) {
                ty.clone()
            } else if ctx.functions.contains_key(name) {
                Type::Unknown
            } else {
                ctx.error_with_hint(
                    format!("unknown variable `{}`", name),
                    "check the spelling or make sure it is defined before this line".into(),
                    *span,
                );
                Type::Error
            }
        }

        Expr::If {
            condition,
            then_body,
            else_body,
            ..
        } => {
            let cond_ty = infer_expr(condition, ctx, ce);
            check_type(
                &cond_ty,
                &Type::Primitive(Primitive::Bool),
                expr_span(expr),
                ctx,
            );
            let mut then_ce = ce.child(Type::Unknown);
            check_body(then_body, ctx, &mut then_ce);
            if let Some(else_stmts) = else_body {
                let mut else_ce = ce.child(Type::Unknown);
                check_body(else_stmts, ctx, &mut else_ce);
            }
            Type::Unknown
        }

        Expr::List { elements, .. } => {
            for e in elements {
                infer_expr(e, ctx, ce);
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
            let mut child = ce.child(Type::Unknown);
            child.loop_depth += 1;
            check_body(body, ctx, &mut child);
            Type::Unit
        }

        Expr::Match {
            subject,
            arms,
            span,
        } => {
            let subject_type = infer_expr(subject, ctx, ce);
            for arm in arms {
                let mut arm_ce = ce.child(Type::Unknown);
                let bound_vars = collect_pattern_bindings(&arm.pattern);
                check_pattern(&arm.pattern, &subject_type, ctx, &mut arm_ce.env);
                if let Some(guard) = &arm.guard {
                    let guard_ty = infer_expr(guard, ctx, &mut arm_ce);
                    check_type(&guard_ty, &Type::Primitive(Primitive::Bool), arm.span, ctx);
                }
                check_body(&arm.body, ctx, &mut arm_ce);
                for (name, name_span) in &bound_vars {
                    if !name.starts_with('_') && !arm_ce.used_vars.contains(name) {
                        ctx.warning(format!("unused variable `{name}`"), *name_span);
                    }
                }
            }
            check_match_exhaustiveness(&subject_type, arms, *span, ctx);
            Type::Unknown
        }

        Expr::MethodCall {
            receiver,
            method,
            args,
            span,
            ..
        } => infer_method_call(receiver, method, args, *span, ctx, ce),

        Expr::ShortClosure { params, body, span } => {
            let mut closure_env = CheckEnv {
                env: HashMap::new(),
                used_vars: HashSet::new(),
                loop_depth: 0,
                return_type: Type::Unknown,
                struct_names: ce.struct_names,
                enum_names: ce.enum_names,
            };
            let param_types = bind_closure_params(params, &mut closure_env, ctx, *span);
            let return_type = infer_expr(body, ctx, &mut closure_env);
            Type::Function {
                params: param_types,
                return_type: Box::new(return_type),
            }
        }

        Expr::Spawn { expr: inner, .. } => {
            infer_expr(inner, ctx, ce);
            Type::Unknown
        }

        Expr::String { .. } => Type::Primitive(Primitive::String),

        Expr::StructConstruction {
            type_path,
            fields,
            span,
        } => infer_struct_construction(type_path, fields, *span, ctx, ce),

        Expr::Ternary {
            condition,
            then_expr,
            else_expr,
            span,
        } => {
            let cond_ty = infer_expr(condition, ctx, ce);
            check_type(&cond_ty, &Type::Primitive(Primitive::Bool), *span, ctx);
            let then_ty = infer_expr(then_expr, ctx, ce);
            let else_ty = infer_expr(else_expr, ctx, ce);
            if then_ty.is_known() && else_ty.is_known() && then_ty != else_ty {
                ctx.error(
                    format!(
                        "ternary branches have different types: `{}` and `{}`",
                        then_ty.display(),
                        else_ty.display()
                    ),
                    *span,
                );
            }
            if then_ty.is_known() { then_ty } else { else_ty }
        }

        Expr::Try { expr: inner, .. } => {
            infer_expr(inner, ctx, ce);
            Type::Unknown
        }

        Expr::Tuple { elements, .. } => {
            let types: Vec<Type> = elements.iter().map(|e| infer_expr(e, ctx, ce)).collect();
            Type::Tuple(types)
        }

        Expr::Unary { op, operand, span } => {
            let operand_ty = infer_expr(operand, ctx, ce);
            match op {
                UnaryOp::Neg => {
                    if operand_ty.is_known() && !operand_ty.is_numeric() {
                        ctx.error_with_hint(
                            format!(
                                "negation requires numeric type, found `{}`",
                                operand_ty.display()
                            ),
                            "expected Int, Int32, Float, or Float32".into(),
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

        Expr::While {
            condition, body, ..
        } => {
            let cond_ty = infer_expr(condition, ctx, ce);
            check_type(
                &cond_ty,
                &Type::Primitive(Primitive::Bool),
                expr_span(expr),
                ctx,
            );
            let mut child = ce.child(Type::Unknown);
            child.loop_depth += 1;
            check_body(body, ctx, &mut child);
            Type::Unit
        }

        Expr::Unless {
            condition, body, ..
        } => {
            let cond_ty = infer_expr(condition, ctx, ce);
            check_type(
                &cond_ty,
                &Type::Primitive(Primitive::Bool),
                expr_span(expr),
                ctx,
            );
            let mut child = ce.child(Type::Unknown);
            check_body(body, ctx, &mut child);
            Type::Unknown
        }

        Expr::Self_ { span } => {
            if let Some(ty) = ce.env.get("self") {
                ty.clone()
            } else {
                ctx.error_with_hint(
                    "`self` used outside of impl block".to_string(),
                    "'self' is only available inside functions defined in an 'impl' block".into(),
                    *span,
                );
                Type::Error
            }
        }

        _ => Type::Unknown,
    }
}

/// Type-checks a binary operation, handling pipe desugaring and arithmetic/comparison/logical ops.
fn infer_binary(
    op: &BinOp,
    left: &Expr,
    right: &Expr,
    span: Span,
    ctx: &mut TypeContext,
    ce: &mut CheckEnv,
) -> Type {
    if *op == BinOp::Pipe {
        infer_expr(left, ctx, ce);
        let func_name = match right {
            Expr::Ident { name, .. } => Some(name.as_str()),
            Expr::Call { callee, args, .. } => {
                for arg in args {
                    infer_expr(&arg.value, ctx, ce);
                }
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    Some(name.as_str())
                } else {
                    None
                }
            }
            _ => None,
        };
        return if let Some(name) = func_name {
            if let Some(sig) = ctx.functions.get(name) {
                sig.return_type.clone()
            } else {
                Type::Unknown
            }
        } else {
            Type::Unknown
        };
    }

    let left_ty = infer_expr(left, ctx, ce);
    let right_ty = infer_expr(right, ctx, ce);

    match op {
        BinOp::Add | BinOp::Div | BinOp::Mod | BinOp::Mul | BinOp::Sub => {
            if left_ty.is_known() && !left_ty.is_numeric() {
                ctx.error_with_hint(
                    format!(
                        "arithmetic requires numeric type, found `{}`",
                        left_ty.display()
                    ),
                    "expected Int, Int32, Float, or Float32".into(),
                    span,
                );
                return Type::Error;
            }
            if left_ty.is_known() && right_ty.is_known() && left_ty != right_ty {
                ctx.error(
                    format!(
                        "type mismatch in arithmetic: `{}` and `{}`",
                        left_ty.display(),
                        right_ty.display()
                    ),
                    span,
                );
                return Type::Error;
            }
            if left_ty.is_known() {
                left_ty
            } else {
                right_ty
            }
        }
        BinOp::And | BinOp::Or => {
            check_type(&left_ty, &Type::Primitive(Primitive::Bool), span, ctx);
            check_type(&right_ty, &Type::Primitive(Primitive::Bool), span, ctx);
            Type::Primitive(Primitive::Bool)
        }
        BinOp::Eq | BinOp::Gt | BinOp::GtEq | BinOp::Lt | BinOp::LtEq | BinOp::NotEq => {
            if left_ty.is_known() && right_ty.is_known() && left_ty != right_ty {
                ctx.error(
                    format!(
                        "cannot compare `{}` and `{}`",
                        left_ty.display(),
                        right_ty.display()
                    ),
                    span,
                );
            }
            Type::Primitive(Primitive::Bool)
        }
        BinOp::Pipe => unreachable!("handled above"),
    }
}

/// Type-checks a function call expression, resolving the callee and validating arguments.
fn infer_call(
    callee: &Expr,
    args: &[Arg],
    span: Span,
    ctx: &mut TypeContext,
    ce: &mut CheckEnv,
) -> Type {
    if let Expr::Ident { name, .. } = callee {
        if let Some(sig) = ctx.functions.get(name).cloned() {
            if !sig.type_params.is_empty() {
                return infer_generic_call(name, &sig, args, span, ctx, ce);
            }
            let return_type = sig.return_type.clone();
            let params = sig.params.clone();
            check_call_args(name, &params, args, "", span, ctx, ce);
            return_type
        } else if ce.env.contains_key(name)
            || ctx.structs.contains_key(name)
            || ctx.enums.contains_key(name)
        {
            for arg in args {
                infer_expr(&arg.value, ctx, ce);
            }
            Type::Unknown
        } else {
            ctx.error(format!("undefined function `{name}`"), span);
            for arg in args {
                infer_expr(&arg.value, ctx, ce);
            }
            Type::Error
        }
    } else {
        infer_expr(callee, ctx, ce);
        for arg in args {
            infer_expr(&arg.value, ctx, ce);
        }
        Type::Unknown
    }
}

fn infer_generic_call(
    name: &str,
    sig: &FunctionSig,
    args: &[Arg],
    span: Span,
    ctx: &mut TypeContext,
    ce: &mut CheckEnv,
) -> Type {
    use crate::types::{substitute, unify};

    if sig.params.len() != args.len() {
        ctx.error(
            format!(
                "`{name}` expects {} argument(s), got {}",
                sig.params.len(),
                args.len()
            ),
            span,
        );
        for arg in args {
            infer_expr(&arg.value, ctx, ce);
        }
        return Type::Error;
    }

    let mut subst = std::collections::HashMap::new();
    for (i, arg) in args.iter().enumerate() {
        let arg_ty = infer_expr(&arg.value, ctx, ce);
        let param_ty = &sig.params[i].ty;
        if arg_ty.is_known() && !unify(param_ty, &arg_ty, &mut subst) {
            ctx.error(
                format!(
                    "argument `{}`: type `{}` conflicts with previous binding for type parameter",
                    sig.params[i].name,
                    arg_ty.display(),
                ),
                arg.span,
            );
            return Type::Error;
        }
    }

    for tp in &sig.type_params {
        if !subst.contains_key(tp) {
            ctx.error(
                format!("cannot infer type parameter `{tp}` for `{name}`"),
                span,
            );
            return Type::Error;
        }
    }

    substitute(&sig.return_type, &subst)
}

/// Type-checks an enum variant construction, validating variant existence and data shape.
/// For generic enums, infers type arguments from constructor values via unification.
fn infer_enum_construction(
    type_path: &[String],
    variant: &str,
    data: &EnumConstructionData,
    span: Span,
    ctx: &mut TypeContext,
    ce: &mut CheckEnv,
) -> Type {
    let enum_name = type_path.join(".");
    if let Some(enum_info) = ctx.enums.get(&enum_name).cloned() {
        if let Some(vi) = enum_info.variants.iter().find(|v| v.name == *variant) {
            let is_generic = !enum_info.type_params.is_empty();
            let mut subst: HashMap<String, Type> = HashMap::new();
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
                            span,
                        );
                    } else {
                        for (i, arg_expr) in args.iter().enumerate() {
                            let arg_ty = infer_expr(arg_expr, ctx, ce);
                            let expected_ty = &expected[i];
                            if is_generic {
                                unify(expected_ty, &arg_ty, &mut subst);
                            } else if expected_ty.is_known()
                                && arg_ty.is_known()
                                && *expected_ty != arg_ty
                            {
                                ctx.error(
                                    format!(
                                        "variant `{}.{}` argument {}: expected `{}`, found `{}`",
                                        enum_name,
                                        variant,
                                        i + 1,
                                        expected_ty.display(),
                                        arg_ty.display()
                                    ),
                                    span,
                                );
                            }
                        }
                    }
                }
                (EnumConstructionData::Struct(fields), VariantData::Struct(expected_fields)) => {
                    for fi in fields {
                        let value_ty = infer_expr(&fi.value, ctx, ce);
                        if let Some((_, field_ty)) =
                            expected_fields.iter().find(|(n, _)| *n == fi.name)
                        {
                            if is_generic {
                                unify(field_ty, &value_ty, &mut subst);
                            } else if field_ty.is_known()
                                && value_ty.is_known()
                                && *field_ty != value_ty
                            {
                                ctx.error(
                                    format!(
                                        "variant `{}.{}` field `{}`: expected `{}`, found `{}`",
                                        enum_name,
                                        variant,
                                        fi.name,
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
                        span,
                    );
                }
                (EnumConstructionData::Tuple(args), _) => {
                    for a in args {
                        infer_expr(a, ctx, ce);
                    }
                    ctx.error(
                        format!(
                            "variant `{}.{}` does not take positional arguments",
                            enum_name, variant
                        ),
                        span,
                    );
                }
                (EnumConstructionData::Struct(fields), _) => {
                    for fi in fields {
                        infer_expr(&fi.value, ctx, ce);
                    }
                    ctx.error(
                        format!(
                            "variant `{}.{}` does not take named fields",
                            enum_name, variant
                        ),
                        span,
                    );
                }
            }
            if is_generic {
                let type_args: Vec<Type> = enum_info
                    .type_params
                    .iter()
                    .map(|tp| subst.get(tp).cloned().unwrap_or(Type::Unknown))
                    .collect();
                Type::GenericInstance {
                    base: enum_name,
                    type_args,
                    kind: GenericKind::Enum,
                }
            } else {
                Type::Enum(enum_name)
            }
        } else {
            ctx.error(
                format!("enum `{}` has no variant `{}`", enum_name, variant),
                span,
            );
            Type::Error
        }
    } else {
        match data {
            EnumConstructionData::Tuple(args) => {
                for a in args {
                    infer_expr(a, ctx, ce);
                }
            }
            EnumConstructionData::Struct(fields) => {
                for fi in fields {
                    infer_expr(&fi.value, ctx, ce);
                }
            }
            EnumConstructionData::Unit => {}
        }
        if ce.enum_names.contains(&enum_name.as_str()) {
            Type::Enum(enum_name)
        } else {
            Type::Unknown
        }
    }
}

/// Type-checks a field access expression, resolving struct fields and reporting mismatches.
fn infer_field_access(
    receiver: &Expr,
    field: &str,
    span: Span,
    ctx: &mut TypeContext,
    ce: &mut CheckEnv,
) -> Type {
    let recv_ty = infer_expr(receiver, ctx, ce);

    let (struct_name, generic_args) = match &recv_ty {
        Type::Struct(name) => (name.as_str(), None),
        Type::GenericInstance {
            base,
            type_args,
            kind: GenericKind::Struct,
        } => (base.as_str(), Some(type_args)),
        Type::Unknown | Type::Error => return recv_ty,
        _ => {
            ctx.error(
                format!("field access on non-struct type `{}`", recv_ty.display()),
                span,
            );
            return Type::Error;
        }
    };

    let Some(struct_info) = ctx.structs.get(struct_name) else {
        return Type::Unknown;
    };

    let Some((_, field_ty)) = struct_info.fields.iter().find(|(n, _)| n == field) else {
        let available: Vec<&str> = struct_info.fields.iter().map(|(n, _)| n.as_str()).collect();
        ctx.error_with_hint(
            format!("struct `{}` has no field `{}`", struct_name, field),
            format!("available fields: {}", available.join(", ")),
            span,
        );
        return Type::Error;
    };

    if let Some(type_args) = generic_args {
        let subst_map: HashMap<String, Type> = struct_info
            .type_params
            .iter()
            .zip(type_args.iter())
            .map(|(p, a)| (p.clone(), a.clone()))
            .collect();
        substitute(field_ty, &subst_map)
    } else {
        field_ty.clone()
    }
}

/// Type-checks a method call, resolving module-qualified calls and struct/enum methods.
fn infer_method_call(
    receiver: &Expr,
    method: &str,
    args: &[Arg],
    span: Span,
    ctx: &mut TypeContext,
    ce: &mut CheckEnv,
) -> Type {
    if let Expr::Ident { name: mod_name, .. } = receiver {
        let mod_lookup = ctx.imported_modules.get(mod_name).map(|mod_ctx| {
            mod_ctx
                .functions
                .get(method)
                .map(|sig| (sig.return_type.clone(), sig.params.clone()))
        });

        match mod_lookup {
            Some(Some((return_type, params))) => {
                let display = format!("{}.{}", mod_name, method);
                check_call_args(&display, &params, args, "", span, ctx, ce);
                return return_type;
            }
            Some(None) => {
                for arg in args {
                    infer_expr(&arg.value, ctx, ce);
                }
                let available: Vec<String> = ctx
                    .imported_modules
                    .get(mod_name)
                    .map(|m| m.functions.keys().cloned().collect())
                    .unwrap_or_default();
                let hint = if available.is_empty() {
                    format!("module `{}` has no public functions", mod_name)
                } else {
                    format!("available functions: {}", available.join(", "))
                };
                ctx.error_with_hint(
                    format!("module `{}` has no function `{}`", mod_name, method),
                    hint,
                    span,
                );
                return Type::Error;
            }
            None => {}
        }
    }

    let recv_ty = infer_expr(receiver, ctx, ce);

    let (method_sig, subst) = match &recv_ty {
        Type::Struct(name) => {
            let direct = ctx
                .structs
                .get(name)
                .and_then(|si| si.methods.get(method))
                .cloned();
            if direct.is_some() {
                (direct, None)
            } else if let Some((base, type_args)) = try_parse_mangled_generic(name, ctx) {
                let info = ctx
                    .structs
                    .get(&base)
                    .map(|si| (&si.methods, &si.type_params));
                if let Some((methods, type_params)) = info {
                    let sig = methods.get(method).cloned();
                    let s = build_substitution(type_params, &type_args);
                    (sig, Some(s))
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            }
        }
        Type::Enum(name) => {
            let direct = ctx
                .enums
                .get(name)
                .and_then(|ei| ei.methods.get(method))
                .cloned();
            if direct.is_some() {
                (direct, None)
            } else if let Some((base, type_args)) = try_parse_mangled_generic(name, ctx) {
                let info = ctx
                    .enums
                    .get(&base)
                    .map(|ei| (&ei.methods, &ei.type_params));
                if let Some((methods, type_params)) = info {
                    let sig = methods.get(method).cloned();
                    let s = build_substitution(type_params, &type_args);
                    (sig, Some(s))
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            }
        }
        Type::GenericInstance {
            base,
            type_args,
            kind,
        } => {
            let info_methods = match kind {
                GenericKind::Struct => ctx
                    .structs
                    .get(base)
                    .map(|si| (&si.methods, &si.type_params)),
                GenericKind::Enum => ctx.enums.get(base).map(|ei| (&ei.methods, &ei.type_params)),
            };
            if let Some((methods, type_params)) = info_methods {
                let sig = methods.get(method).cloned();
                let s = build_substitution(type_params, type_args);
                (sig, Some(s))
            } else {
                (None, None)
            }
        }
        _ => (None, None),
    };

    if let Some(sig) = method_sig {
        let (return_type, params) = if let Some(ref s) = subst {
            let ret = substitute(&sig.return_type, s);
            let ps: Vec<_> = sig
                .params
                .iter()
                .map(|p| ParamInfo {
                    name: p.name.clone(),
                    ty: substitute(&p.ty, s),
                })
                .collect();
            (ret, ps)
        } else {
            (sig.return_type.clone(), sig.params.clone())
        };
        check_call_args(method, &params, args, "self, ", span, ctx, ce);
        return_type
    } else {
        for arg in args {
            infer_expr(&arg.value, ctx, ce);
        }
        match &recv_ty {
            Type::Struct(name) => {
                let available: Vec<&str> = ctx
                    .structs
                    .get(name)
                    .map(|s| s.methods.keys().map(|k| k.as_str()).collect())
                    .unwrap_or_default();
                let hint = if available.is_empty() {
                    format!("struct `{}` has no functions defined", name)
                } else {
                    format!("available functions: {}", available.join(", "))
                };
                ctx.error_with_hint(
                    format!("struct `{}` has no function `{}`", name, method),
                    hint,
                    span,
                );
                Type::Error
            }
            Type::Enum(name) => {
                let available: Vec<&str> = ctx
                    .enums
                    .get(name)
                    .map(|e| e.methods.keys().map(|k| k.as_str()).collect())
                    .unwrap_or_default();
                let hint = if available.is_empty() {
                    format!("enum `{}` has no functions defined", name)
                } else {
                    format!("available functions: {}", available.join(", "))
                };
                ctx.error_with_hint(
                    format!("enum `{}` has no function `{}`", name, method),
                    hint,
                    span,
                );
                Type::Error
            }
            Type::GenericInstance { base, kind, .. } => {
                let kind_str = match kind {
                    GenericKind::Struct => "struct",
                    GenericKind::Enum => "enum",
                };
                let methods_map = match kind {
                    GenericKind::Struct => ctx.structs.get(base).map(|si| &si.methods),
                    GenericKind::Enum => ctx.enums.get(base).map(|ei| &ei.methods),
                };
                let available: Vec<&str> = methods_map
                    .map(|m| m.keys().map(|k| k.as_str()).collect())
                    .unwrap_or_default();
                let hint = if available.is_empty() {
                    format!("{} `{}` has no functions defined", kind_str, base)
                } else {
                    format!("available functions: {}", available.join(", "))
                };
                ctx.error_with_hint(
                    format!("{} `{}` has no function `{}`", kind_str, base, method),
                    hint,
                    span,
                );
                Type::Error
            }
            _ => Type::Unknown,
        }
    }
}

/// Type-checks a struct construction expression, validating fields and their types.
fn infer_struct_construction(
    type_path: &[String],
    fields: &[FieldInit],
    span: Span,
    ctx: &mut TypeContext,
    ce: &mut CheckEnv,
) -> Type {
    let name = type_path.join(".");
    if let Some(struct_info) = ctx.structs.get(&name) {
        let struct_fields = struct_info.fields.clone();
        for fi in fields {
            let value_ty = infer_expr(&fi.value, ctx, ce);
            if let Some((_, field_ty)) = struct_fields.iter().find(|(n, _)| *n == fi.name) {
                if field_ty.is_known() && value_ty.is_known() && *field_ty != value_ty {
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
                let available: Vec<&str> = struct_fields.iter().map(|(n, _)| n.as_str()).collect();
                ctx.error_with_hint(
                    format!("struct `{}` has no field `{}`", name, fi.name),
                    format!("available fields: {}", available.join(", ")),
                    fi.span,
                );
            }
        }
        Type::Struct(name)
    } else {
        for fi in fields {
            infer_expr(&fi.value, ctx, ce);
        }
        if ce.struct_names.contains(&name.as_str()) {
            Type::Struct(name)
        } else {
            ctx.error(format!("unknown struct `{}`", name), span);
            Type::Error
        }
    }
}

/// Checks whether a match expression covers all variants of an enum subject,
/// emitting a diagnostic if any variants are missing and no catch-all exists.
fn check_match_exhaustiveness(
    subject_type: &Type,
    arms: &[MatchArm],
    span: Span,
    ctx: &mut TypeContext,
) {
    let Type::Enum(enum_name) = subject_type else {
        return;
    };
    let Some(enum_info) = ctx.enums.get(enum_name) else {
        return;
    };

    let has_catch_all = arms.iter().any(|arm| {
        matches!(
            arm.pattern,
            Pattern::Wildcard { .. } | Pattern::Binding { .. }
        ) && arm.guard.is_none()
    });
    if has_catch_all {
        return;
    }

    let matched: Vec<&str> = arms
        .iter()
        .filter(|arm| arm.guard.is_none())
        .filter_map(|arm| match &arm.pattern {
            Pattern::EnumUnit { variant, .. }
            | Pattern::EnumTuple { variant, .. }
            | Pattern::EnumStruct { variant, .. } => Some(variant.as_str()),
            Pattern::Constructor { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();

    let missing: Vec<&str> = enum_info
        .variants
        .iter()
        .filter(|v| !matched.contains(&v.name.as_str()))
        .map(|v| v.name.as_str())
        .collect();

    if !missing.is_empty() {
        let missing_list = missing.join(", ");
        ctx.error_with_hint(
            format!(
                "non-exhaustive match: missing variant(s) `{}`",
                missing_list
            ),
            format!("add a `_ ->` catch-all or handle: {}", missing_list),
            span,
        );
    }
}

/// Resolves the variant data for a pattern, applying type substitution for
/// generic enums when the subject type is a `GenericInstance`.
fn resolve_variant_data(
    enum_name: &str,
    variant: &str,
    subject_type: &Type,
    ctx: &TypeContext,
) -> Option<VariantData> {
    let enum_info = ctx.enums.get(enum_name)?;
    let vi = enum_info.variants.iter().find(|v| v.name == *variant)?;
    let data = vi.data.clone();

    if let Type::GenericInstance {
        type_args,
        kind: GenericKind::Enum,
        ..
    } = subject_type
        && !enum_info.type_params.is_empty()
    {
        let subst = crate::types::build_substitution(&enum_info.type_params, type_args);
        return Some(substitute_variant_data(&data, &subst));
    }
    Some(data)
}

fn substitute_variant_data(data: &VariantData, subst: &HashMap<String, Type>) -> VariantData {
    match data {
        VariantData::Unit => VariantData::Unit,
        VariantData::Tuple(types) => {
            VariantData::Tuple(types.iter().map(|t| substitute(t, subst)).collect())
        }
        VariantData::Struct(fields) => VariantData::Struct(
            fields
                .iter()
                .map(|(n, t)| (n.clone(), substitute(t, subst)))
                .collect(),
        ),
    }
}

/// Recursively validates a match pattern against the expected subject type,
/// binding pattern variables into the environment.
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
            let variant_data = resolve_variant_data(&enum_name, variant, subject_type, ctx);

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
            let variant_data = resolve_variant_data(&enum_name, variant, subject_type, ctx);

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
            if lit_type.is_known() && subject_type.is_known() && lit_type != *subject_type {
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

/// Validates that call arguments match the expected parameter count and types,
/// emitting diagnostics for arity mismatches or type mismatches.
fn check_call_args(
    display_name: &str,
    params: &[ParamInfo],
    args: &[Arg],
    sig_prefix: &str,
    span: Span,
    ctx: &mut TypeContext,
    ce: &mut CheckEnv,
) {
    if params.len() != args.len() {
        let param_list: Vec<String> = params
            .iter()
            .map(|p| format!("{}: {}", p.name, p.ty.display()))
            .collect();
        ctx.error_with_hint(
            format!(
                "function `{}` expects {} argument(s), got {}",
                display_name,
                params.len(),
                args.len()
            ),
            format!(
                "signature: fn {}({}{})",
                display_name,
                sig_prefix,
                param_list.join(", ")
            ),
            span,
        );
    } else {
        for (i, arg) in args.iter().enumerate() {
            let arg_ty = infer_expr(&arg.value, ctx, ce);
            let param = &params[i];
            if param.ty.is_known() && arg_ty.is_known() && param.ty != arg_ty {
                ctx.error(
                    format!(
                        "argument `{}`: expected `{}`, found `{}`",
                        param.name,
                        param.ty.display(),
                        arg_ty.display()
                    ),
                    arg.span,
                );
            }
        }
    }
}

fn check_type(actual: &Type, expected: &Type, span: Span, ctx: &mut TypeContext) {
    if !actual.is_known() || !expected.is_known() {
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

fn bind_closure_params(
    params: &[ClosureParam],
    ce: &mut CheckEnv,
    ctx: &mut TypeContext,
    _closure_span: Span,
) -> Vec<Type> {
    let mut types = Vec::new();
    for p in params {
        match p {
            ClosureParam::Name {
                name, type_expr, ..
            } => {
                let ty = if let Some(te) = type_expr {
                    resolve_type_expr(te, ce.struct_names, ce.enum_names)
                } else {
                    Type::Unknown
                };
                ce.env.insert(name.clone(), ty.clone());
                types.push(ty);
            }
            ClosureParam::Destructured { names, span, .. } => {
                ctx.error_with_hint(
                    "destructured closure parameters are not yet supported".to_string(),
                    "use individual named parameters instead".into(),
                    *span,
                );
                for name in names {
                    ce.env.insert(name.clone(), Type::Unknown);
                    types.push(Type::Unknown);
                }
            }
            ClosureParam::Wildcard { .. } => {
                types.push(Type::Unknown);
            }
        }
    }
    types
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
        | Expr::Unless { span, .. }
        | Expr::While { span, .. } => *span,
    }
}

fn collect_pattern_bindings(pat: &Pattern) -> Vec<(String, Span)> {
    let mut bindings = Vec::new();
    collect_bindings_inner(pat, &mut bindings);
    bindings
}

fn collect_bindings_inner(pat: &Pattern, out: &mut Vec<(String, Span)>) {
    match pat {
        Pattern::Binding { name, span, .. } => {
            out.push((name.clone(), *span));
        }
        Pattern::EnumTuple { elements, .. }
        | Pattern::Tuple { elements, .. }
        | Pattern::Constructor { elements, .. }
        | Pattern::List { elements, .. } => {
            for sub in elements {
                collect_bindings_inner(sub, out);
            }
        }
        Pattern::EnumStruct { fields, .. } => {
            for f in fields {
                if let Some(sub) = &f.pattern {
                    collect_bindings_inner(sub, out);
                } else {
                    out.push((f.name.clone(), f.span));
                }
            }
        }
        Pattern::Wildcard { .. } | Pattern::Literal { .. } | Pattern::EnumUnit { .. } => {}
    }
}

fn numeric_compatible(a: &Type, b: &Type) -> bool {
    if let (Type::Primitive(pa), Type::Primitive(pb)) = (a, b) {
        (pa.is_integer() && pb.is_integer()) || (pa.is_float() && pb.is_float())
    } else {
        false
    }
}

/// Attempts to parse a mangled generic name (e.g. `Pair_$i32.i32$`) back into
/// the base name and concrete type arguments for method resolution.
fn try_parse_mangled_generic(name: &str, ctx: &TypeContext) -> Option<(String, Vec<Type>)> {
    let sep_pos = name.find("_$")?;
    let base = &name[..sep_pos];
    if !ctx.structs.contains_key(base) && !ctx.enums.contains_key(base) {
        return None;
    }
    if !name.ends_with('$') {
        return None;
    }
    let inner = &name[sep_pos + 2..name.len() - 1];
    let type_args: Vec<Type> = inner
        .split('.')
        .map(|s| {
            use crate::types::Primitive;
            if let Some(p) = Primitive::from_name(s) {
                Type::Primitive(p)
            } else if s == "unit" {
                Type::Unit
            } else {
                Type::Struct(s.to_string())
            }
        })
        .collect();
    Some((base.to_string(), type_args))
}

fn types_compatible(a: &Type, b: &Type) -> bool {
    if a == b || numeric_compatible(a, b) {
        return true;
    }
    if let (
        Type::GenericInstance {
            base: ba,
            type_args: ta,
            ..
        },
        Type::GenericInstance {
            base: bb,
            type_args: tb,
            ..
        },
    ) = (a, b)
    {
        return ba == bb
            && ta.len() == tb.len()
            && ta
                .iter()
                .zip(tb.iter())
                .all(|(x, y)| !x.is_known() || !y.is_known() || x == y);
    }
    false
}
