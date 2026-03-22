//! Expression type inference.
//!
//! Walks expression AST nodes to infer their types, emitting diagnostics for
//! type mismatches, undefined variables, and invalid operations. Handles all
//! expression forms including calls, closures, field access, method dispatch,
//! and enum/struct construction.

use std::collections::{HashMap, HashSet};

use expo_ast::ast::*;
use expo_ast::span::Span;

use crate::check::{check_call_args, check_type, try_parse_mangled_generic, types_compatible};
use crate::context::{
    CaptureInfo, FunctionKind, FunctionSig, ParamInfo, PassMode, TypeContext, VariantData,
};
use crate::env::{CheckEnv, VarInfo};
use crate::pattern::{check_match_exhaustiveness, check_pattern, collect_pattern_bindings};
use crate::stmt::check_body;
use crate::types::{
    GenericKind, Primitive, Type, build_substitution, resolve_type_expr, substitute, unify,
    unwrap_indirect,
};

/// Infers the type of an expression, emitting diagnostics for any type errors
/// encountered during traversal. Returns `Type::Unknown` when the type cannot
/// be determined.
pub(crate) fn infer_expr(expr: &Expr, ctx: &mut TypeContext, ce: &mut CheckEnv) -> Type {
    match expr {
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
            let parent_var_names: HashSet<String> = ce.env.keys().cloned().collect();

            let mut closure_env = CheckEnv {
                env: ce.env.clone(),
                used_vars: HashSet::new(),
                loop_depth: 0,
                return_type: Type::Unknown,
                kind: FunctionKind::Static,
                struct_names: ce.struct_names,
                enum_names: ce.enum_names,
                type_hint: None,
                process_msg_type: ce.process_msg_type.clone(),
            };
            let param_types = bind_closure_params(params, &mut closure_env, ctx, *span);

            let param_names: HashSet<String> = params
                .iter()
                .filter_map(|p| {
                    if let ClosureParam::Name { name, .. } = p {
                        Some(name.clone())
                    } else {
                        None
                    }
                })
                .collect();

            check_body(body, ctx, &mut closure_env);
            let return_type = body
                .last()
                .and_then(|s| match s {
                    Statement::Expr(e) => Some(infer_expr(e, ctx, &mut closure_env)),
                    _ => None,
                })
                .unwrap_or(Type::Unit);

            let capture_candidates: Vec<(String, Type)> = closure_env
                .used_vars
                .iter()
                .filter(|name| parent_var_names.contains(*name) && !param_names.contains(*name))
                .filter_map(|name| ce.env.get(name).map(|info| (name.clone(), info.ty.clone())))
                .collect();

            let mut captured = Vec::new();
            for (name, ty) in capture_candidates {
                let mode = if ty.is_copy() {
                    PassMode::Copy
                } else {
                    ce.mark_moved(&name, *span);
                    PassMode::Move
                };
                captured.push(CaptureInfo { name, ty, mode });
            }

            if !captured.is_empty() {
                ctx.closure_captures.insert(*span, captured);
            }

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

        Expr::For {
            pattern,
            iterable,
            body,
            span,
        } => {
            let iter_ty = infer_expr(iterable, ctx, ce);
            let elem_ty = resolve_enumerable_element_type(&iter_ty, ctx).unwrap_or_else(|| {
                if iter_ty.is_known() {
                    ctx.error(
                        format!(
                            "`for` requires an Enumeration type, found `{}`",
                            iter_ty.display()
                        ),
                        *span,
                    );
                }
                Type::Unknown
            });
            let mut child = ce.child(Type::Unknown);
            child.loop_depth += 1;
            let bindings = collect_pattern_bindings(pattern);
            for (name, _) in &bindings {
                child.insert_var(name.clone(), elem_ty.clone());
            }
            check_body(body, ctx, &mut child);
            Type::Unit
        }

        Expr::Group { expr: inner, .. } => infer_expr(inner, ctx, ce),

        Expr::Ident { name, span } => {
            if let Some(info) = ce.env.get(name) {
                ce.check_not_moved(name, *span, ctx);
                ce.used_vars.insert(name.clone());
                info.ty.clone()
            } else if let Some(ty) = ctx.constants.get(name) {
                ty.clone()
            } else if let Some(sig) = ctx.functions.get(name) {
                if sig.type_params.is_empty() {
                    Type::Function {
                        params: sig.params.iter().map(|p| p.ty.clone()).collect(),
                        return_type: Box::new(sig.return_type.clone()),
                    }
                } else {
                    Type::Unknown
                }
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
                ce.merge_branches(&[then_ce.env, else_ce.env]);
            } else {
                ce.merge_branches(&[then_ce.env]);
            }
            Type::Unknown
        }

        Expr::List { elements, span } => {
            let mut elem_type = Type::Unknown;
            for e in elements {
                let t = infer_expr(e, ctx, ce);
                if elem_type == Type::Unknown {
                    elem_type = t;
                } else if t.is_known() && elem_type.is_known() && !types_compatible(&elem_type, &t)
                {
                    ctx.error(
                        format!(
                            "list element type mismatch: expected `{}`, found `{}`",
                            elem_type.display(),
                            t.display()
                        ),
                        *span,
                    );
                }
            }
            Type::GenericInstance {
                base: "List".to_string(),
                type_args: vec![elem_type],
                kind: GenericKind::Struct,
            }
        }

        Expr::Map { entries, span } => {
            let mut key_type = Type::Unknown;
            let mut val_type = Type::Unknown;
            for (k, v) in entries {
                let kt = infer_expr(k, ctx, ce);
                let vt = infer_expr(v, ctx, ce);
                if key_type == Type::Unknown {
                    key_type = kt;
                } else if kt.is_known() && key_type.is_known() && !types_compatible(&key_type, &kt)
                {
                    ctx.error(
                        format!(
                            "map key type mismatch: expected `{}`, found `{}`",
                            key_type.display(),
                            kt.display()
                        ),
                        *span,
                    );
                }
                if val_type == Type::Unknown {
                    val_type = vt;
                } else if vt.is_known() && val_type.is_known() && !types_compatible(&val_type, &vt)
                {
                    ctx.error(
                        format!(
                            "map value type mismatch: expected `{}`, found `{}`",
                            val_type.display(),
                            vt.display()
                        ),
                        *span,
                    );
                }
            }
            Type::GenericInstance {
                base: "Map".to_string(),
                type_args: vec![key_type, val_type],
                kind: GenericKind::Struct,
            }
        }

        Expr::Literal { value, .. } => match value {
            Literal::Bool(_) => Type::Primitive(Primitive::Bool),
            Literal::Float(_) => Type::Primitive(Primitive::F64),
            Literal::Int(_) => Type::Primitive(Primitive::I64),
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
                env: HashMap::<String, VarInfo>::new(),
                used_vars: HashSet::new(),
                loop_depth: 0,
                return_type: Type::Unknown,
                kind: FunctionKind::Static,
                struct_names: ce.struct_names,
                enum_names: ce.enum_names,
                type_hint: None,
                process_msg_type: ce.process_msg_type.clone(),
            };
            let param_types = bind_closure_params(params, &mut closure_env, ctx, *span);
            let return_type = infer_expr(body, ctx, &mut closure_env);
            Type::Function {
                params: param_types,
                return_type: Box::new(return_type),
            }
        }

        Expr::Spawn { expr: inner, span } => {
            let type_name = match inner.as_ref() {
                Expr::MethodCall {
                    receiver, method, ..
                } if method == "new" => {
                    if let Expr::Ident { name, .. } = receiver.as_ref() {
                        Some(name.clone())
                    } else {
                        None
                    }
                }
                Expr::Call { callee, .. } => match callee.as_ref() {
                    Expr::FieldAccess {
                        receiver, field, ..
                    } if field == "new" => {
                        if let Expr::Ident { name, .. } = receiver.as_ref() {
                            Some(name.clone())
                        } else {
                            None
                        }
                    }
                    _ => None,
                },
                _ => None,
            };

            let Some(target) = type_name else {
                ctx.error(
                    "spawn requires `Type.new(config)` form where Type implements Process"
                        .to_string(),
                    *span,
                );
                return Type::Unknown;
            };

            let process_args = ctx.protocol_impls.get(&target).and_then(|impls| {
                impls
                    .iter()
                    .find(|(proto, _)| proto == "Process")
                    .map(|(_, args)| args.clone())
            });

            let Some(args) = process_args else {
                ctx.error(
                    format!("`{target}` does not implement the Process protocol"),
                    *span,
                );
                return Type::Unknown;
            };

            if args.len() < 3 {
                ctx.error(
                    format!("Process impl for `{target}` is missing type arguments"),
                    *span,
                );
                return Type::Unknown;
            }

            let msg_type = args[1].clone();
            let reply_type_template = args[2].clone();

            let inner_ty = infer_expr(inner, ctx, ce);

            let reply_type = match &inner_ty {
                Type::GenericInstance {
                    base, type_args, ..
                } if *base == target && type_args.len() == 1 => type_args[0].clone(),
                Type::Struct(name) => {
                    if let Some((base, type_args)) = try_parse_mangled_generic(name, ctx)
                        && base == target
                        && type_args.len() == 1
                    {
                        type_args[0].clone()
                    } else {
                        reply_type_template.clone()
                    }
                }
                _ => reply_type_template.clone(),
            };

            Type::GenericInstance {
                base: "Ref".to_string(),
                type_args: vec![msg_type, reply_type],
                kind: crate::types::GenericKind::Struct,
            }
        }

        Expr::Receive {
            arms,
            after_timeout,
            after_body,
            ..
        } => {
            let subject_type = ce
                .process_msg_type
                .clone()
                .unwrap_or(Type::Primitive(Primitive::String));
            for arm in arms {
                let mut arm_ce = ce.child(Type::Unknown);
                check_pattern(&arm.pattern, &subject_type, ctx, &mut arm_ce.env);
                if let Some(guard) = &arm.guard {
                    let guard_ty = infer_expr(guard, ctx, &mut arm_ce);
                    check_type(&guard_ty, &Type::Primitive(Primitive::Bool), arm.span, ctx);
                }
                check_body(&arm.body, ctx, &mut arm_ce);
            }
            if let Some(timeout) = after_timeout {
                infer_expr(timeout, ctx, ce);
            }
            for stmt in after_body {
                crate::stmt::check_statement(stmt, ctx, ce);
            }
            subject_type
        }

        Expr::String { parts, .. } => {
            for part in parts {
                if let StringPart::Interpolation { expr, .. } = part {
                    infer_expr(expr, ctx, ce);
                }
            }
            Type::Primitive(Primitive::String)
        }

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
            if let Some(ty) = ce.get_type("self") {
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
            if left_ty.is_known() && right_ty.is_known() && !types_compatible(&left_ty, &right_ty) {
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
            if left_ty.is_known() && right_ty.is_known() && !types_compatible(&left_ty, &right_ty) {
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

/// Whether an argument type should drive generic parameter unification.
/// [`Type::is_known`] is false for [`Type::GenericInstance`], but concrete
/// instances like `Ref<(), Int>` must still unify with `Ref<(), R>` to bind `R`.
fn arg_ty_participates_in_unification(ty: &Type) -> bool {
    !matches!(unwrap_indirect(ty), Type::Unknown | Type::Error)
}

/// If `ty` is a monomorphized struct name (`Ref_$unit.Int$`), expand to [`Type::GenericInstance`]
/// so it unifies with generic signatures that still use [`Type::GenericInstance`].
fn expand_mangled_generic_type(ty: &Type, ctx: &TypeContext) -> Type {
    let ty = match ty {
        Type::Indirect(inner) => inner.as_ref(),
        other => other,
    };
    match ty {
        Type::Struct(name) => {
            if let Some((base, type_args)) = try_parse_mangled_generic(name, ctx) {
                let kind = if ctx.structs.contains_key(&base) {
                    GenericKind::Struct
                } else {
                    GenericKind::Enum
                };
                Type::GenericInstance {
                    base,
                    kind,
                    type_args,
                }
            } else {
                ty.clone()
            }
        }
        _ => ty.clone(),
    }
}

/// Infers the return type of a generic function call via type parameter unification.
fn infer_generic_call(
    name: &str,
    sig: &FunctionSig,
    args: &[Arg],
    span: Span,
    ctx: &mut TypeContext,
    ce: &mut CheckEnv,
) -> Type {
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

    let mut subst = HashMap::new();
    for (i, arg) in args.iter().enumerate() {
        let arg_ty = infer_expr(&arg.value, ctx, ce);
        let arg_ty = expand_mangled_generic_type(&arg_ty, ctx);
        let param_ty = &sig.params[i].ty;
        if arg_ty_participates_in_unification(&arg_ty) && !unify(param_ty, &arg_ty, &mut subst) {
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

    if sig.type_params.iter().any(|tp| !subst.contains_key(tp))
        && let Some(hint) = &ce.type_hint
    {
        unify(&sig.return_type, hint, &mut subst);
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
                                && !types_compatible(expected_ty, &arg_ty)
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
                                && !types_compatible(field_ty, &value_ty)
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
    let effective_ty = match &recv_ty {
        Type::Indirect(inner) => inner.as_ref(),
        other => other,
    };

    let (struct_name, generic_args) = match effective_ty {
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

    if let Expr::Ident {
        name: type_name, ..
    } = receiver
    {
        let static_sig_info = ctx
            .structs
            .get(type_name)
            .map(|si| (&si.methods, &si.type_params))
            .or_else(|| {
                ctx.enums
                    .get(type_name)
                    .map(|ei| (&ei.methods, &ei.type_params))
            })
            .and_then(|(methods, type_params)| {
                methods.get(method).and_then(|sig| {
                    if sig.kind == FunctionKind::Static {
                        Some((sig.clone(), type_params.clone()))
                    } else {
                        None
                    }
                })
            });

        if let Some((sig, type_params)) = static_sig_info {
            if !sig.type_params.is_empty() || !type_params.is_empty() {
                let static_sig = FunctionSig {
                    visibility: sig.visibility,
                    params: sig.params.clone(),
                    return_type: sig.return_type.clone(),
                    kind: sig.kind,
                    span: sig.span,
                    type_params: if !type_params.is_empty() {
                        type_params
                    } else {
                        sig.type_params.clone()
                    },
                };
                let display = format!("{}.{}", type_name, method);
                return infer_generic_call(&display, &static_sig, args, span, ctx, ce);
            }
            let display = format!("{}.{}", type_name, method);
            check_call_args(&display, &sig.params, args, "", span, ctx, ce);
            return sig.return_type.clone();
        }
    }

    let recv_ty_raw = infer_expr(receiver, ctx, ce);
    let recv_ty = match &recv_ty_raw {
        Type::Indirect(inner) => inner.as_ref().clone(),
        other => other.clone(),
    };

    if method == "clone" && args.is_empty() {
        return recv_ty;
    }

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
                    mode: p.mode,
                    name: p.name.clone(),
                    ty: substitute(&p.ty, s),
                })
                .collect();
            (ret, ps)
        } else {
            (sig.return_type.clone(), sig.params.clone())
        };

        if sig.kind == FunctionKind::Instance(PassMode::Move)
            && !recv_ty.is_copy()
            && let Expr::Ident { name, .. } = receiver
        {
            ce.mark_moved(name, span);
        }

        if !sig.type_params.is_empty() {
            let method_sig_for_infer = FunctionSig {
                visibility: sig.visibility,
                params,
                return_type,
                kind: sig.kind,
                span: sig.span,
                type_params: sig.type_params.clone(),
            };
            return infer_generic_call(method, &method_sig_for_infer, args, span, ctx, ce);
        }

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
                if field_ty.is_known()
                    && value_ty.is_known()
                    && !types_compatible(field_ty, &value_ty)
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

/// Binds closure parameters into the type-checking environment, returning their types.
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
                ce.insert_var(name.clone(), ty.clone());
                types.push(ty);
            }
            ClosureParam::Destructured { names, span, .. } => {
                ctx.error_with_hint(
                    "destructured closure parameters are not yet supported".to_string(),
                    "use individual named parameters instead".into(),
                    *span,
                );
                for name in names {
                    ce.insert_var(name.clone(), Type::Unknown);
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

/// Returns the source span of an expression node.
pub(crate) fn expr_span(expr: &Expr) -> Span {
    match expr {
        Expr::Arena { span, .. }
        | Expr::Binary { span, .. }
        | Expr::BinaryLiteral { span, .. }
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
        | Expr::Map { span, .. }
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
        | Expr::Unary { span, .. }
        | Expr::Unless { span, .. }
        | Expr::While { span, .. } => *span,
    }
}

/// Resolves the element type for any type that implements the `Enumeration<T>`
/// protocol by looking up the `get` method and substituting concrete type args.
fn resolve_enumerable_element_type(ty: &Type, ctx: &TypeContext) -> Option<Type> {
    let (base, type_args) = match ty {
        Type::GenericInstance {
            base, type_args, ..
        } => (base.clone(), type_args.clone()),
        Type::Struct(name) => {
            if let Some((base, type_args)) = try_parse_mangled_generic(name, ctx) {
                (base, type_args)
            } else {
                return None;
            }
        }
        _ => return None,
    };

    let protos = ctx.protocol_impls.get(&base)?;
    if !protos.iter().any(|(p, _)| p == "Enumeration") {
        return None;
    }

    let struct_info = ctx.structs.get(&base)?;
    let get_sig = struct_info.methods.get("get")?;
    let subst = build_substitution(&struct_info.type_params, &type_args);
    Some(substitute(&get_sig.return_type, &subst))
}
