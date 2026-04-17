//! Expression type inference.
//!
//! Walks expression AST nodes to infer their types, emitting diagnostics for
//! type mismatches, undefined variables, and invalid operations. Handles all
//! expression forms including calls, closures, field access, method dispatch,
//! and enum/struct construction.

use std::collections::{HashMap, HashSet};

use expo_ast::ast::*;
use expo_ast::identifier::TypeIdentifier;
use expo_ast::span::Span;

use crate::check::{
    check_call_args, check_literal_overflow, check_type, try_parse_mangled_generic,
    types_compatible,
};
use crate::context::{
    CaptureInfo, ClosureInfo, FnParam, FunctionKind, FunctionSig, ParamInfo, PassMode, TypeContext,
    VariantData,
};
use crate::env::CheckEnv;
use crate::pattern::{check_match_exhaustiveness, check_pattern, collect_pattern_bindings};
use crate::stmt::{check_body, check_statement};
use crate::types::{
    Primitive, Type, build_substitution, named, named_generic, resolve_type_alias_id,
    resolve_type_alias_name, substitute_preserving, unify, unwrap_indirect,
};

/// Infers the type of an expression, emitting diagnostics for any type errors
/// encountered during traversal. Returns `Type::Unknown` when the type cannot
/// be determined.
pub(crate) fn infer_expr(expr: &mut Expr, ctx: &mut TypeContext, ce: &mut CheckEnv) -> Type {
    let span = expr.span;
    let ty = match &mut expr.kind {
        ExprKind::Binary {
            op, left, right, ..
        } => infer_binary(op, left, right, span, ctx, ce),

        ExprKind::Call { callee, args, .. } => infer_call(callee, args, span, ctx, ce),

        ExprKind::Closure { params, body, .. } => infer_closure(params, None, body, span, ctx, ce),

        ExprKind::Cond { arms, else_body } => {
            let mut arm_types: Vec<Type> = Vec::new();
            for arm in arms {
                infer_expr(&mut arm.condition, ctx, ce);
                let mut child = ce.child(Type::Unknown);
                arm_types.push(infer_body_type(&mut arm.body, ctx, &mut child));
            }
            if let Some(body) = else_body {
                let mut child = ce.child(Type::Unknown);
                arm_types.push(infer_body_type(body, ctx, &mut child));
            }

            let result_type = arm_types
                .iter()
                .find(|t| t.is_known() && **t != Type::Unit)
                .cloned()
                .or_else(|| arm_types.iter().find(|t| t.is_known()).cloned())
                .unwrap_or(Type::Unknown);

            if result_type.is_known() && result_type != Type::Unit {
                for arm_ty in &arm_types {
                    if arm_ty.is_known() && *arm_ty != result_type {
                        ctx.error(
                            format!(
                                "cond arms have inconsistent types: expected `{}`, got `{}`",
                                result_type.display(),
                                arm_ty.display()
                            ),
                            span,
                        );
                        break;
                    }
                }
            }

            result_type
        }

        ExprKind::EnumConstruction {
            type_path,
            variant,
            data,
        } => infer_enum_construction(type_path, variant, data, span, ctx, ce),

        ExprKind::FieldAccess { receiver, field } => {
            infer_field_access(receiver, field, span, ctx, ce)
        }

        ExprKind::For {
            pattern,
            iterable,
            body,
        } => {
            let iter_ty = infer_expr(iterable, ctx, ce);
            let elem_ty = resolve_enumerable_element_type(&iter_ty, ctx).unwrap_or_else(|| {
                if iter_ty.is_known() {
                    ctx.error(
                        format!(
                            "`for` requires an Enumeration type, found `{}`",
                            iter_ty.display()
                        ),
                        span,
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

        ExprKind::Group { expr: inner, .. } => infer_expr(inner, ctx, ce),

        ExprKind::Ident { name } => {
            if let Some(info) = ce.env.get(name) {
                ce.check_not_moved(name, span, ctx);
                ce.used_vars.insert(name.clone());
                info.ty.clone()
            } else if let Some(ty) = ctx.constants.get(name) {
                ty.clone()
            } else if let Some(sig) = ctx.functions.get(name) {
                if sig.type_params.is_empty() {
                    Type::Function {
                        params: sig.params.iter().map(FnParam::from).collect(),
                        return_type: Box::new(sig.return_type.clone()),
                    }
                } else {
                    Type::Unknown
                }
            } else {
                ctx.error_with_hint(
                    format!("unknown variable `{}`", name),
                    "check the spelling or make sure it is defined before this line".into(),
                    span,
                );
                Type::Error
            }
        }

        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            let cond_ty = infer_expr(condition, ctx, ce);
            check_type(&cond_ty, &Type::Primitive(Primitive::Bool), span, ctx);
            let mut then_ce = ce.child(Type::Unknown);
            let then_ty = infer_body_type(then_body, ctx, &mut then_ce);
            if let Some(else_stmts) = else_body {
                let mut else_ce = ce.child(Type::Unknown);
                let else_ty = infer_body_type(else_stmts, ctx, &mut else_ce);
                ce.merge_branches(&[then_ce.env, else_ce.env]);
                if then_ty.is_known() { then_ty } else { else_ty }
            } else {
                ce.merge_branches(&[then_ce.env]);
                Type::Unit
            }
        }

        ExprKind::List { elements } => {
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
                        span,
                    );
                }
            }
            named_generic("List", vec![elem_type], ctx, ctx.current_package.as_ref())
        }

        ExprKind::Map { entries } => {
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
                        span,
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
                        span,
                    );
                }
            }
            named_generic(
                "Map",
                vec![key_type, val_type],
                ctx,
                ctx.current_package.as_ref(),
            )
        }

        ExprKind::Literal { value, .. } => match value {
            Literal::Bool(_) => Type::Primitive(Primitive::Bool),
            Literal::Float(_) => Type::Primitive(Primitive::F64),
            Literal::Int(_) => Type::Primitive(Primitive::I64),
            Literal::String(_) => Type::Primitive(Primitive::String),
            Literal::Unit => Type::Unit,
        },

        ExprKind::Loop { body, .. } => {
            let mut child = ce.child(Type::Unknown);
            child.loop_depth += 1;
            check_body(body, ctx, &mut child);
            Type::Unit
        }

        ExprKind::Match { subject, arms } => {
            let subject_type = infer_expr(subject, ctx, ce);
            let mut result_type = Type::Unknown;
            for arm in arms.iter_mut() {
                let mut arm_ce = ce.child(Type::Unknown);
                let bound_vars = collect_pattern_bindings(&arm.pattern);
                check_pattern(&mut arm.pattern, &subject_type, ctx, &mut arm_ce.env);
                if let Some(guard) = &mut arm.guard {
                    let guard_ty = infer_expr(guard, ctx, &mut arm_ce);
                    check_type(&guard_ty, &Type::Primitive(Primitive::Bool), arm.span, ctx);
                }
                let arm_ty = infer_body_type(&mut arm.body, ctx, &mut arm_ce);
                if result_type == Type::Unknown && arm_ty.is_known() {
                    result_type = arm_ty;
                }
                for (name, name_span) in &bound_vars {
                    if !name.starts_with('_') && !arm_ce.used_vars.contains(name) {
                        ctx.warning(format!("unused variable `{name}`"), *name_span);
                    }
                }
            }
            check_match_exhaustiveness(&subject_type, arms, span, ctx);
            result_type
        }

        ExprKind::MethodCall {
            receiver,
            method,
            args,
        } => infer_method_call(receiver, method, args, span, ctx, ce),

        ExprKind::ShortClosure { params, body, .. } => {
            infer_short_closure(params, None, body, span, ctx, ce)
        }

        ExprKind::Spawn { expr: inner } => {
            let type_name = match &inner.kind {
                ExprKind::MethodCall {
                    receiver, method, ..
                } if method == "start" => {
                    if let ExprKind::Ident { name, .. } = &receiver.kind {
                        Some(name.clone())
                    } else {
                        None
                    }
                }
                ExprKind::Call { callee, .. } => match &callee.kind {
                    ExprKind::FieldAccess {
                        receiver, field, ..
                    } if field == "start" => {
                        if let ExprKind::Ident { name, .. } = &receiver.kind {
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
                    "spawn requires `Type.start(config)` form where Type implements Process"
                        .to_string(),
                    span,
                );
                return Type::Unknown;
            };

            let process_args = ctx
                .resolve_name(&target)
                .cloned()
                .and_then(|id| ctx.protocol_impls.get(&id).cloned())
                .and_then(|impls| {
                    impls
                        .iter()
                        .find(|(proto, _)| proto == "Process")
                        .map(|(_, args)| args.clone())
                });

            let Some(args) = process_args else {
                ctx.error(
                    format!("`{target}` does not implement the Process protocol"),
                    span,
                );
                return Type::Unknown;
            };

            if args.len() < 3 {
                ctx.error(
                    format!("Process impl for `{target}` is missing type arguments"),
                    span,
                );
                return Type::Unknown;
            }

            let msg_type = args[1].clone();
            let reply_type_template = args[2].clone();

            let inner_ty = infer_expr(inner, ctx, ce);

            let state_ty = match &inner_ty {
                Type::Named {
                    identifier,
                    type_args,
                } if identifier.name == "Result" => {
                    type_args.first().cloned().unwrap_or(inner_ty.clone())
                }
                _ => inner_ty.clone(),
            };

            let reply_type = match &state_ty {
                Type::Named {
                    identifier,
                    type_args,
                } if identifier.name == target && type_args.len() == 1 => type_args[0].clone(),
                Type::Named {
                    identifier,
                    type_args,
                } if type_args.is_empty() => {
                    if let Some((base, type_args)) =
                        try_parse_mangled_generic(&identifier.name, ctx)
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

            named_generic(
                "Ref",
                vec![msg_type, reply_type],
                ctx,
                ctx.current_package.as_ref(),
            )
        }

        ExprKind::Receive {
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
                check_pattern(&mut arm.pattern, &subject_type, ctx, &mut arm_ce.env);
                if let Some(guard) = &mut arm.guard {
                    let guard_ty = infer_expr(guard, ctx, &mut arm_ce);
                    check_type(&guard_ty, &Type::Primitive(Primitive::Bool), arm.span, ctx);
                }
                check_body(&mut arm.body, ctx, &mut arm_ce);
            }
            if let Some(timeout) = after_timeout {
                infer_expr(timeout, ctx, ce);
            }
            for stmt in after_body {
                crate::stmt::check_statement(stmt, ctx, ce);
            }
            subject_type
        }

        ExprKind::String { parts, .. } => {
            for part in parts {
                if let StringPart::Interpolation { expr, .. } = part {
                    infer_expr(&mut *expr, ctx, ce);
                }
            }
            Type::Primitive(Primitive::String)
        }

        ExprKind::StructConstruction { type_path, fields } => {
            infer_struct_construction(type_path, fields, span, ctx, ce)
        }

        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            let cond_ty = infer_expr(condition, ctx, ce);
            check_type(&cond_ty, &Type::Primitive(Primitive::Bool), span, ctx);
            let then_ty = infer_expr(then_expr, ctx, ce);
            let else_ty = infer_expr(else_expr, ctx, ce);
            if then_ty.is_known() && else_ty.is_known() && then_ty != else_ty {
                ctx.error(
                    format!(
                        "ternary branches have different types: `{}` and `{}`",
                        then_ty.display(),
                        else_ty.display()
                    ),
                    span,
                );
            }
            if then_ty.is_known() { then_ty } else { else_ty }
        }

        ExprKind::Unary { op, operand } => {
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
                            span,
                        );
                        Type::Error
                    } else {
                        operand_ty
                    }
                }
                UnaryOp::Not => {
                    check_type(&operand_ty, &Type::Primitive(Primitive::Bool), span, ctx);
                    Type::Primitive(Primitive::Bool)
                }
            }
        }

        ExprKind::While { condition, body } => {
            let cond_ty = infer_expr(condition, ctx, ce);
            check_type(&cond_ty, &Type::Primitive(Primitive::Bool), span, ctx);
            let mut child = ce.child(Type::Unknown);
            child.loop_depth += 1;
            check_body(body, ctx, &mut child);
            Type::Unit
        }

        ExprKind::Unless { condition, body } => {
            let cond_ty = infer_expr(condition, ctx, ce);
            check_type(&cond_ty, &Type::Primitive(Primitive::Bool), span, ctx);
            let mut child = ce.child(Type::Unknown);
            check_body(body, ctx, &mut child);
            Type::Unknown
        }

        ExprKind::BinaryLiteral { segments } => infer_binary_literal(segments, span, ctx, ce),

        ExprKind::Self_ => {
            if let Some(ty) = ce.get_type("self") {
                ty.clone()
            } else {
                ctx.error_with_hint(
                    "`self` used outside of impl block".to_string(),
                    "'self' is only available inside functions defined in an 'impl' block".into(),
                    span,
                );
                Type::Error
            }
        }

        ExprKind::Arena { .. } => Type::Unknown,
    };
    expr.resolved_type = Some(ty.clone());
    ty
}

/// Checks a statement list and infers the type of its last expression.
/// Non-expression trailing statements yield `Type::Unit`.
fn infer_body_type(body: &mut [Statement], ctx: &mut TypeContext, ce: &mut CheckEnv) -> Type {
    if body.is_empty() {
        return Type::Unit;
    }
    let len = body.len();
    check_body(&mut body[..len - 1], ctx, ce);
    match body.last_mut().unwrap() {
        Statement::Expr(expr) => infer_expr(expr, ctx, ce),
        stmt => {
            check_statement(stmt, ctx, ce);
            Type::Unit
        }
    }
}

/// Type-checks a binary operation, handling pipe desugaring and arithmetic/comparison/logical ops.
fn infer_binary(
    op: &BinOp,
    left: &mut Expr,
    right: &mut Expr,
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
        BinOp::Concat => {
            let is_concat_type = |ty: &Type| {
                matches!(
                    ty,
                    Type::Primitive(Primitive::String)
                        | Type::Primitive(Primitive::Binary)
                        | Type::Primitive(Primitive::Bits)
                )
            };
            if left_ty.is_known() && !is_concat_type(&left_ty) {
                ctx.error(
                    format!(
                        "`<>` requires String, Binary, or Bits, found `{}`",
                        left_ty.display()
                    ),
                    span,
                );
                return Type::Error;
            }
            if left_ty.is_known() && right_ty.is_known() && !types_compatible(&left_ty, &right_ty) {
                ctx.error(
                    format!(
                        "`<>` requires both sides to be the same type, found `{}` and `{}`",
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
    }
}

/// Type-checks a binary/bitstring literal (`<<segments...>>`), validating each
/// segment and computing whether the result is `Binary` (byte-aligned) or
/// `Bits` (non-byte-aligned).
fn infer_binary_literal(
    segments: &mut [BinarySegment],
    _span: Span,
    ctx: &mut TypeContext,
    ce: &mut CheckEnv,
) -> Type {
    if segments.is_empty() {
        return Type::Primitive(Primitive::Binary);
    }

    let mut total_bits: Option<u64> = Some(0);

    for seg in segments {
        let val_ty = infer_expr(&mut seg.value, ctx, ce);

        let seg_bits: Option<u64> = if let Some(size_expr) = &mut seg.size {
            let size_ty = infer_expr(size_expr, ctx, ce);
            if size_ty.is_known() && !matches!(size_ty, Type::Primitive(p) if p.is_integer()) {
                ctx.error(
                    format!(
                        "segment size must be an integer, found `{}`",
                        size_ty.display()
                    ),
                    seg.span,
                );
            }

            if let ExprKind::Literal {
                value: Literal::Int(n),
            } = &size_expr.kind
            {
                if let Ok(bits) = n.parse::<u64>() {
                    let actual_bits = if seg.unit == BinaryUnit::Byte {
                        bits * 8
                    } else {
                        bits
                    };
                    check_literal_overflow(&seg.value, actual_bits, seg.signedness, seg.span, ctx);
                    Some(actual_bits)
                } else {
                    None
                }
            } else {
                None
            }
        } else if let Some(type_ann) = &seg.type_ann {
            let ann_ty = ctx.resolve_type_annotation(type_ann, ce.struct_names, ce.enum_names);
            match &ann_ty {
                Type::Primitive(p) => {
                    if let Some(w) = p.bit_width() {
                        Some(w)
                    } else {
                        if *p != Primitive::Binary && *p != Primitive::Bits {
                            ctx.error(
                                format!(
                                    "type `{}` has no fixed bit width, use a concrete type like Int32 or Float64",
                                    p.display()
                                ),
                                seg.span,
                            );
                        }
                        None
                    }
                }
                Type::Unknown => None,
                _ => {
                    ctx.error(
                        format!(
                            "segment type annotation must be a primitive type, found `{}`",
                            ann_ty.display()
                        ),
                        seg.span,
                    );
                    None
                }
            }
        } else if matches!(val_ty, Type::Primitive(Primitive::String)) {
            if seg.signedness.is_some() || seg.endianness.is_some() {
                ctx.error(
                    "string segments do not support signedness or endianness modifiers".to_string(),
                    seg.span,
                );
            }
            if let ExprKind::String { parts, .. } = &seg.value.kind {
                let byte_len: usize = parts
                    .iter()
                    .map(|p| match p {
                        StringPart::Literal { value, .. } => value.len(),
                        _ => 0,
                    })
                    .sum();
                Some(byte_len as u64 * 8)
            } else {
                None
            }
        } else {
            if val_ty.is_known() && !matches!(val_ty, Type::Primitive(p) if p.is_integer()) {
                ctx.error(
                    format!(
                        "default segment value must be an integer (8-bit), found `{}`",
                        val_ty.display()
                    ),
                    seg.span,
                );
            }
            check_literal_overflow(&seg.value, 8, None, seg.span, ctx);
            Some(8)
        };

        if seg.signedness.is_some() && seg.size.is_none() && seg.type_ann.is_none() {
            ctx.error(
                "signedness modifier requires a size specifier (::N)".to_string(),
                seg.span,
            );
        }
        if seg.endianness.is_some() && seg.size.is_none() && seg.type_ann.is_none() {
            ctx.error(
                "endianness modifier requires a size specifier (::N)".to_string(),
                seg.span,
            );
        }

        match (total_bits, seg_bits) {
            (Some(acc), Some(b)) => total_bits = Some(acc + b),
            _ => total_bits = None,
        }
    }

    match total_bits {
        Some(n) if n.is_multiple_of(8) => Type::Primitive(Primitive::Binary),
        Some(_) => Type::Primitive(Primitive::Bits),
        None => Type::Primitive(Primitive::Binary),
    }
}

/// Type-checks a function call expression, resolving the callee and validating arguments.
fn infer_call(
    callee: &mut Expr,
    args: &mut [Arg],
    span: Span,
    ctx: &mut TypeContext,
    ce: &mut CheckEnv,
) -> Type {
    if let ExprKind::Ident { name, .. } = &callee.kind {
        if let Some(sig) = ctx.functions.get(name).cloned() {
            if !sig.type_params.is_empty() {
                return infer_generic_call(name, &sig, args, span, ctx, ce);
            }
            let return_type = sig.return_type.clone();
            let params = sig.params.clone();
            check_call_args(name, &params, args, "", span, ctx, ce);
            return_type
        } else if let Some(Type::Function {
            params,
            return_type,
        }) = ce.get_type(name).cloned()
        {
            let param_infos: Vec<ParamInfo> = params
                .iter()
                .enumerate()
                .map(|(i, fp)| ParamInfo {
                    mode: fp.mode,
                    name: format!("_{i}"),
                    ty: fp.ty.clone(),
                })
                .collect();
            check_call_args(name, &param_infos, args, "", span, ctx, ce);
            *return_type
        } else if let Some(sig) = ce
            .enclosing_type
            .as_ref()
            .and_then(|id| ctx.get_type(id))
            .and_then(|ti| ti.functions.get(name))
            .cloned()
        {
            if !sig.type_params.is_empty() {
                return infer_generic_call(name, &sig, args, span, ctx, ce);
            }
            let return_type = sig.return_type.clone();
            let params = sig.params.clone();
            check_call_args(name, &params, args, "", span, ctx, ce);
            return_type
        } else if ce.env.contains_key(name) || ctx.resolve_name(name).is_some() {
            for arg in args {
                infer_expr(&mut arg.value, ctx, ce);
            }
            Type::Unknown
        } else {
            ctx.error(format!("undefined function `{name}`"), span);
            for arg in args {
                infer_expr(&mut arg.value, ctx, ce);
            }
            Type::Error
        }
    } else {
        infer_expr(callee, ctx, ce);
        for arg in args {
            infer_expr(&mut arg.value, ctx, ce);
        }
        Type::Unknown
    }
}

fn is_closure_expr(expr: &Expr) -> bool {
    matches!(
        expr.kind,
        ExprKind::ShortClosure { .. } | ExprKind::Closure { .. }
    )
}

/// Whether an argument type should drive generic parameter unification.
/// [`Type::is_known`] is false for [`Type::Named`] with unknown args, but concrete
/// instances like `Ref<(), Int>` must still unify with `Ref<(), R>` to bind `R`.
fn arg_ty_participates_in_unification(ty: &Type) -> bool {
    !matches!(unwrap_indirect(ty), Type::Unknown | Type::Error)
}

/// If `ty` is a monomorphized named type (`Ref_$unit.Int$`), expand to [`Type::Named`]
/// with populated type_args so it unifies with generic signatures.
fn expand_mangled_generic_type(ty: &Type, ctx: &TypeContext) -> Type {
    let ty = match ty {
        Type::Indirect(inner) => inner.as_ref(),
        other => other,
    };
    match ty {
        Type::Named {
            identifier,
            type_args,
        } if type_args.is_empty() => {
            if let Some((base, type_args)) = try_parse_mangled_generic(&identifier.name, ctx) {
                named_generic(&base, type_args, ctx, ctx.current_package.as_ref())
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
    args: &mut [Arg],
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
            infer_expr(&mut arg.value, ctx, ce);
        }
        return Type::Error;
    }

    let mut subst = HashMap::new();

    // Pass 1: infer non-closure arguments to bind type parameters
    for (i, arg) in args.iter_mut().enumerate() {
        if is_closure_expr(&arg.value) {
            continue;
        }
        let arg_ty = infer_expr(&mut arg.value, ctx, ce);
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

    // Pass 2: infer closure arguments with partially-substituted expected types
    for (i, arg) in args.iter_mut().enumerate() {
        if !is_closure_expr(&arg.value) {
            continue;
        }
        let param_ty = &sig.params[i].ty;
        let expected = substitute_preserving(param_ty, &subst);
        let arg_ty = infer_expr_with_expected(&mut arg.value, Some(&expected), ctx, ce);
        let arg_ty = expand_mangled_generic_type(&arg_ty, ctx);
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

    if sig
        .type_params
        .iter()
        .any(|tp| !subst.contains_key(&tp.name))
        && let Some(hint) = &ce.type_hint
    {
        unify(&sig.return_type, hint, &mut subst);
    }

    for tp in &sig.type_params {
        if !subst.contains_key(&tp.name) {
            ctx.error(
                format!("cannot infer type parameter `{}` for `{name}`", tp.name),
                span,
            );
            return Type::Error;
        }
    }

    for tp in &sig.type_params {
        if tp.bounds.is_empty() {
            continue;
        }
        let Some(concrete) = subst.get(&tp.name) else {
            continue;
        };
        let (type_name, type_id) = match concrete {
            Type::Named { identifier, .. } => {
                (Some(identifier.name.clone()), Some(identifier.clone()))
            }
            Type::Primitive(p) => (Some(p.display().to_string()), None),
            _ => (None, None),
        };
        let Some(type_name) = type_name else {
            continue;
        };
        let id = type_id.or_else(|| ctx.resolve_name(&type_name).cloned());
        let impls = id.as_ref().and_then(|id| ctx.protocol_impls.get(id));
        for bound in &tp.bounds {
            let satisfied = impls.is_some_and(|list| list.iter().any(|(proto, _)| proto == bound));
            if !satisfied {
                ctx.error(
                    format!(
                        "type `{}` does not implement protocol `{bound}` (required by type parameter `{}` in `{name}`)",
                        concrete.display(),
                        tp.name,
                    ),
                    span,
                );
                return Type::Error;
            }
        }
    }

    substitute_preserving(&sig.return_type, &subst)
}

/// Type-checks an enum variant construction, validating variant existence and data shape.
/// For generic enums, infers type arguments from constructor values via unification.
fn infer_enum_construction(
    type_path: &[String],
    variant: &str,
    data: &mut EnumConstructionData,
    span: Span,
    ctx: &mut TypeContext,
    ce: &mut CheckEnv,
) -> Type {
    let enum_name = type_path.join(".");
    // Aliases are file-local; consult `type_aliases` first so single-segment
    // aliases like `AlphaStatus` resolve to their package-qualified origin
    // before the generic `find_type` bare-name lookup.
    let aliased_type_info = if type_path.len() == 1 {
        resolve_type_alias_id(&enum_name, &ctx.type_aliases)
            .and_then(|id| ctx.get_type(&id).cloned())
    } else {
        None
    };
    let looked_up = aliased_type_info.or_else(|| ctx.find_type(&enum_name).cloned());
    if let Some(type_info) = looked_up.filter(|ti| ti.is_enum()) {
        let enum_variants = type_info.variants().unwrap();
        if let Some(vi) = enum_variants.iter().find(|v| v.name == *variant) {
            let is_generic = !type_info.type_params.is_empty();
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
                        for (i, arg_expr) in args.iter_mut().enumerate() {
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
                        let value_ty = infer_expr(&mut fi.value, ctx, ce);
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
                        infer_expr(&mut fi.value, ctx, ce);
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
                let type_args: Vec<Type> = type_info
                    .type_params
                    .iter()
                    .map(|tp| subst.get(&tp.name).cloned().unwrap_or(Type::Unknown))
                    .collect();
                Type::Named {
                    identifier: type_info.identifier.clone(),
                    type_args,
                }
            } else {
                Type::Named {
                    identifier: type_info.identifier.clone(),
                    type_args: vec![],
                }
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
                    infer_expr(&mut fi.value, ctx, ce);
                }
            }
            EnumConstructionData::Unit => {}
        }
        if ce.enum_names.contains(&enum_name.as_str()) {
            ctx.lookup_by_name(&enum_name)
                .filter(|ti| ti.is_enum())
                .map(|ti| Type::Named {
                    identifier: ti.identifier.clone(),
                    type_args: vec![],
                })
                .unwrap_or_else(|| named(&enum_name))
        } else {
            Type::Unknown
        }
    }
}

/// Type-checks a field access expression, resolving struct fields and reporting mismatches.
fn infer_field_access(
    receiver: &mut Expr,
    field: &str,
    span: Span,
    ctx: &mut TypeContext,
    ce: &mut CheckEnv,
) -> Type {
    let recv_ty = infer_expr(receiver, ctx, ce);
    if field.is_empty() {
        return recv_ty;
    }
    let effective_ty = match &recv_ty {
        Type::Indirect(inner) => inner.as_ref(),
        other => other,
    };

    let (struct_id, generic_args) = match effective_ty {
        Type::Named {
            identifier,
            type_args,
        } => {
            if type_args.is_empty() {
                (identifier, None)
            } else {
                (identifier, Some(type_args))
            }
        }
        Type::Unknown | Type::Error => return recv_ty,
        _ => {
            ctx.error(
                format!("field access on non-struct type `{}`", recv_ty.display()),
                span,
            );
            return Type::Error;
        }
    };

    let Some(type_info) = ctx.get_type(struct_id) else {
        return Type::Unknown;
    };

    let Some(fields) = type_info.fields() else {
        return Type::Unknown;
    };

    let Some((_, field_ty)) = fields.iter().find(|(n, _)| n == field) else {
        let available: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
        ctx.error_with_hint(
            format!("struct `{}` has no field `{}`", struct_id.name, field),
            format!("available fields: {}", available.join(", ")),
            span,
        );
        return Type::Error;
    };

    if let Some(type_args) = generic_args {
        let subst_map: HashMap<String, Type> = type_info
            .type_params
            .iter()
            .zip(type_args.iter())
            .map(|(p, a)| (p.name.clone(), a.clone()))
            .collect();
        substitute_preserving(field_ty, &subst_map)
    } else {
        field_ty.clone()
    }
}

/// Maps a receiver type to the [`TypeIdentifier`] used for function lookup in
/// `ctx.types`, plus an optional generic substitution map when the receiver is
/// a generic instance.
pub(crate) fn resolve_receiver_base_name(
    recv_ty: &Type,
    ctx: &TypeContext,
) -> (Option<TypeIdentifier>, Option<HashMap<String, Type>>) {
    match recv_ty {
        Type::Named {
            identifier,
            type_args,
        } => {
            if type_args.is_empty() {
                if ctx.get_type(identifier).is_some() {
                    (Some(identifier.clone()), None)
                } else if let Some(resolved) = ctx.resolve_name(&identifier.name) {
                    (Some(resolved.clone()), None)
                } else if let Some((base, type_args)) =
                    try_parse_mangled_generic(&identifier.name, ctx)
                {
                    let base_id = ctx.resolve_name(&base).cloned();
                    let subst = base_id
                        .as_ref()
                        .and_then(|id| ctx.get_type(id))
                        .map(|ti| build_substitution(&ti.type_params, &type_args));
                    (base_id, subst)
                } else {
                    (None, None)
                }
            } else {
                let resolved_id = if ctx.get_type(identifier).is_some() {
                    identifier.clone()
                } else {
                    ctx.resolve_name(&identifier.name)
                        .cloned()
                        .unwrap_or_else(|| identifier.clone())
                };
                let subst = ctx
                    .get_type(&resolved_id)
                    .map(|ti| build_substitution(&ti.type_params, type_args));
                (Some(resolved_id), subst)
            }
        }
        Type::Primitive(p) => (ctx.resolve_name(p.display()).cloned(), None),
        Type::Pointer(inner) => {
            let id = ctx
                .resolve_name("CPtr")
                .cloned()
                .unwrap_or_else(|| TypeIdentifier::unresolved("CPtr"));
            let subst = ctx
                .get_type(&id)
                .map(|ti| build_substitution(&ti.type_params, &[*inner.clone()]));
            (Some(id), subst)
        }
        _ => (None, None),
    }
}

/// Type-checks a method call, resolving type functions.
fn infer_method_call(
    receiver: &mut Expr,
    method: &str,
    args: &mut [Arg],
    span: Span,
    ctx: &mut TypeContext,
    ce: &mut CheckEnv,
) -> Type {
    if let ExprKind::Ident {
        name: type_name, ..
    } = &receiver.kind
    {
        let resolved_name = resolve_type_alias_name(type_name, &ctx.type_aliases);
        let type_info = resolve_type_alias_id(type_name, &ctx.type_aliases)
            .and_then(|id| ctx.get_type(&id))
            .or_else(|| ctx.find_type(&resolved_name));
        let static_sig_info = type_info.and_then(|ti| {
            ti.functions.get(method).and_then(|sig| {
                if sig.kind == FunctionKind::Static {
                    Some((sig.clone(), ti.type_params.clone()))
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

    if let Type::Parameter(ref tv_name) = recv_ty
        && let Some(tp) = ce.fn_type_params.iter().find(|tp| &tp.name == tv_name)
    {
        for bound in &tp.bounds {
            if let Some(pi) = ctx.protocols.get(bound)
                && let Some(proto_sig) = pi.methods.get(method)
            {
                let subst_self =
                    HashMap::from([("Self".to_string(), Type::Parameter(tv_name.clone()))]);
                let return_type = substitute_preserving(&proto_sig.return_type, &subst_self);
                let params: Vec<ParamInfo> = proto_sig
                    .params
                    .iter()
                    .map(|p| ParamInfo {
                        mode: p.mode,
                        name: p.name.clone(),
                        ty: substitute_preserving(&p.ty, &subst_self),
                    })
                    .collect();
                check_call_args(method, &params, args, "self, ", span, ctx, ce);
                return return_type;
            }
        }
        let bound_list = tp.bounds.join(" & ");
        ctx.error_with_hint(
            format!("cannot call `{method}` on type `{tv_name}`"),
            format!(
                "add a `: {bound_list}` bound that provides `{method}`, or use a different method"
            ),
            span,
        );
        return Type::Error;
    }

    let (base_id, subst) = resolve_receiver_base_name(&recv_ty, ctx);

    let recv_type_args: Vec<Type> = match &recv_ty {
        Type::Named { type_args, .. } => type_args.clone(),
        Type::Pointer(inner) => vec![*inner.clone()],
        _ => Vec::new(),
    };

    let found_specialized = base_id.as_ref().and_then(|id| {
        ctx.specialized_methods.get(id).and_then(|entries| {
            entries
                .iter()
                .find(|(concrete_args, _)| *concrete_args == recv_type_args)
                .and_then(|(_, sigs)| sigs.get(method).cloned())
        })
    });
    let is_specialized = found_specialized.is_some();

    let has_specialization_elsewhere = !is_specialized
        && base_id.as_ref().is_some_and(|id| {
            ctx.specialized_methods
                .get(id)
                .is_some_and(|entries| entries.iter().any(|(_, sigs)| sigs.contains_key(method)))
        });

    let method_sig = found_specialized.or_else(|| {
        base_id
            .as_ref()
            .and_then(|id| ctx.get_type(id))
            .and_then(|ti| ti.functions.get(method))
            .cloned()
    });

    if let Some(sig) = method_sig {
        let (return_type, params) = if let Some(ref s) = subst
            && !is_specialized
        {
            let ret = substitute_preserving(&sig.return_type, s);
            let ps: Vec<_> = sig
                .params
                .iter()
                .map(|p| ParamInfo {
                    mode: p.mode,
                    name: p.name.clone(),
                    ty: substitute_preserving(&p.ty, s),
                })
                .collect();
            (ret, ps)
        } else {
            (sig.return_type.clone(), sig.params.clone())
        };

        if sig.kind == FunctionKind::Instance(PassMode::Move)
            && !recv_ty.is_copy()
            && let ExprKind::Ident { name, .. } = &receiver.kind
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
            infer_expr(&mut arg.value, ctx, ce);
        }
        if has_specialization_elsewhere {
            if let Some(id) = &base_id {
                let available_on: Vec<String> = ctx
                    .specialized_methods
                    .get(id)
                    .into_iter()
                    .flat_map(|entries| entries.iter())
                    .filter(|(_, sigs)| sigs.contains_key(method))
                    .map(|(args, _)| {
                        let args_str = args
                            .iter()
                            .map(|t| t.display())
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("{}<{}>", id.name, args_str)
                    })
                    .collect();
                let hint = format!(
                    "`{method}` is only available on {}",
                    available_on.join(", ")
                );
                ctx.error_with_hint(
                    format!(
                        "`{}` has no function `{method}` for this type specialization",
                        id.name
                    ),
                    hint,
                    span,
                );
            }
            Type::Error
        } else if let Some(id) = &base_id {
            let ti = ctx.get_type(id);
            let kind_label = ti.map(|t| t.kind_label()).unwrap_or("type");
            let available: Vec<&str> = ti
                .map(|t| t.functions.keys().map(|k| k.as_str()).collect())
                .unwrap_or_default();
            let name = &id.name;
            let hint = if available.is_empty() {
                format!("{kind_label} `{name}` has no functions defined")
            } else {
                format!("available functions: {}", available.join(", "))
            };
            ctx.error_with_hint(
                format!("{kind_label} `{name}` has no function `{method}`"),
                hint,
                span,
            );
            Type::Error
        } else {
            Type::Unknown
        }
    }
}

/// Type-checks a struct construction expression, validating fields and their types.
fn infer_struct_construction(
    type_path: &[String],
    fields: &mut [FieldInit],
    span: Span,
    ctx: &mut TypeContext,
    ce: &mut CheckEnv,
) -> Type {
    let name = type_path.join(".");
    let lookup = ctx
        .find_type(&name)
        .map(|ti| (ti.identifier.clone(), ti.fields().cloned()));
    if let Some((resolved_id, Some(struct_fields))) = lookup {
        for fi in fields {
            let value_ty = infer_expr(&mut fi.value, ctx, ce);
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
        Type::Named {
            identifier: resolved_id,
            type_args: vec![],
        }
    } else {
        for fi in fields {
            infer_expr(&mut fi.value, ctx, ce);
        }
        if ce.struct_names.contains(&name.as_str()) {
            ctx.lookup_by_name(&name)
                .filter(|ti| ti.is_struct())
                .map(|ti| Type::Named {
                    identifier: ti.identifier.clone(),
                    type_args: vec![],
                })
                .unwrap_or_else(|| named(&name))
        } else {
            ctx.error(format!("unknown struct `{}`", name), span);
            Type::Error
        }
    }
}

/// Like [`infer_expr`], but propagates an expected type into closure arguments.
/// When the expression is a closure and `expected` is `Type::Function`, the
/// expected parameter types are used to fill in unannotated closure parameters.
pub(crate) fn infer_expr_with_expected(
    expr: &mut Expr,
    expected: Option<&Type>,
    ctx: &mut TypeContext,
    ce: &mut CheckEnv,
) -> Type {
    let expected_params = match expected {
        Some(Type::Function { params, .. }) => Some(params.as_slice()),
        _ => None,
    };

    if !matches!(
        &expr.kind,
        ExprKind::ShortClosure { .. } | ExprKind::Closure { .. }
    ) {
        return infer_expr(expr, ctx, ce);
    }

    let span = expr.span;
    let ty = match &mut expr.kind {
        ExprKind::ShortClosure { params, body, .. } => {
            infer_short_closure(params, expected_params, body, span, ctx, ce)
        }
        ExprKind::Closure { params, body, .. } => {
            infer_closure(params, expected_params, body, span, ctx, ce)
        }
        _ => unreachable!(),
    };
    expr.resolved_type = Some(ty.clone());
    ty
}

/// Shared inference for block closures (`fn (params) -> Type ... end`).
fn infer_closure(
    params: &[ClosureParam],
    expected_param_types: Option<&[FnParam]>,
    body: &mut [Statement],
    span: Span,
    ctx: &mut TypeContext,
    ce: &mut CheckEnv,
) -> Type {
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
        fn_type_params: ce.fn_type_params.clone(),
        enclosing_type: ce.enclosing_type.clone(),
    };
    let fn_params = bind_closure_params(params, expected_param_types, &mut closure_env, ctx, span);

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
        .last_mut()
        .and_then(|s| match s {
            Statement::Expr(e) => Some(infer_expr(e, ctx, &mut closure_env)),
            _ => None,
        })
        .unwrap_or(Type::Unit);

    let captured = collect_captures(&closure_env, &parent_var_names, &param_names, ce, span);

    let site = (ctx.current_module_path.clone(), span);
    ctx.closure_info.insert(
        site,
        ClosureInfo {
            captures: captured,
            param_types: fn_params.iter().map(|fp| fp.ty.clone()).collect(),
            return_type: None,
        },
    );

    Type::Function {
        params: fn_params,
        return_type: Box::new(return_type),
    }
}

/// Shared inference for short closures (`x -> expr`).
fn infer_short_closure(
    params: &[ClosureParam],
    expected_param_types: Option<&[FnParam]>,
    body: &mut Expr,
    span: Span,
    ctx: &mut TypeContext,
    ce: &mut CheckEnv,
) -> Type {
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
        fn_type_params: ce.fn_type_params.clone(),
        enclosing_type: ce.enclosing_type.clone(),
    };
    let fn_params = bind_closure_params(params, expected_param_types, &mut closure_env, ctx, span);

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

    let return_type = infer_expr(body, ctx, &mut closure_env);

    let captured = collect_captures(&closure_env, &parent_var_names, &param_names, ce, span);

    let site = (ctx.current_module_path.clone(), span);
    ctx.closure_info.insert(
        site,
        ClosureInfo {
            captures: captured,
            param_types: fn_params.iter().map(|fp| fp.ty.clone()).collect(),
            return_type: Some(return_type.clone()),
        },
    );

    Type::Function {
        params: fn_params,
        return_type: Box::new(return_type),
    }
}

/// Collects captured variables from a closure's environment.
fn collect_captures(
    closure_env: &CheckEnv,
    parent_var_names: &HashSet<String>,
    param_names: &HashSet<String>,
    ce: &mut CheckEnv,
    span: Span,
) -> Vec<CaptureInfo> {
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
            ce.mark_moved(&name, span);
            PassMode::Move
        };
        captured.push(CaptureInfo { name, ty, mode });
    }
    captured
}

/// Binds closure parameters into the type-checking environment, returning their types.
/// When `expected_param_types` is provided (from the calling context's function signature),
/// unannotated params use the expected type instead of `Unknown`. Explicit annotations
/// always take priority.
fn bind_closure_params(
    params: &[ClosureParam],
    expected_param_types: Option<&[FnParam]>,
    ce: &mut CheckEnv,
    ctx: &mut TypeContext,
    _closure_span: Span,
) -> Vec<FnParam> {
    let mut result = Vec::new();
    for (i, p) in params.iter().enumerate() {
        let expected = expected_param_types.and_then(|e: &[FnParam]| e.get(i));
        match p {
            ClosureParam::Name {
                mode,
                name,
                type_expr,
                ..
            } => {
                let ty = if let Some(te) = type_expr {
                    ctx.resolve_type_annotation(te, ce.struct_names, ce.enum_names)
                } else if let Some(exp) = expected {
                    exp.ty.clone()
                } else {
                    Type::Unknown
                };
                ce.insert_var(name.clone(), ty.clone());
                result.push(FnParam { ty, mode: *mode });
            }
            ClosureParam::Destructured { names, span, .. } => {
                ctx.error_with_hint(
                    "destructured closure parameters are not yet supported".to_string(),
                    "use individual named parameters instead".into(),
                    *span,
                );
                for name in names {
                    ce.insert_var(name.clone(), Type::Unknown);
                    result.push(FnParam::borrow(Type::Unknown));
                }
            }
            ClosureParam::Wildcard { .. } => {
                let fp = expected
                    .cloned()
                    .unwrap_or_else(|| FnParam::borrow(Type::Unknown));
                result.push(fp);
            }
        }
    }
    result
}

/// Returns the source span of an expression node.
pub(crate) fn expr_span(expr: &Expr) -> Span {
    expr.span
}

/// Resolves the element type for any type that implements the `Enumeration<T>`
/// protocol by looking up the `get` method and substituting concrete type args.
fn resolve_enumerable_element_type(ty: &Type, ctx: &TypeContext) -> Option<Type> {
    let (base, type_args) = match ty {
        Type::Named {
            identifier,
            type_args,
        } => {
            if type_args.is_empty() {
                if let Some((base, type_args)) = try_parse_mangled_generic(&identifier.name, ctx) {
                    (base, type_args)
                } else {
                    (identifier.name.clone(), vec![])
                }
            } else {
                (identifier.name.clone(), type_args.clone())
            }
        }
        Type::Primitive(p) => (p.display().to_string(), Vec::new()),
        _ => return None,
    };

    let protos_id = ctx.resolve_name(&base)?;
    let protos = ctx.protocol_impls.get(protos_id)?;
    if !protos.iter().any(|(p, _)| p == "Enumeration") {
        return None;
    }

    let ti = ctx.find_type(&base)?;
    let get_sig = ti.functions.get("get")?;

    let option_ty = if type_args.is_empty() {
        get_sig.return_type.clone()
    } else {
        let subst = build_substitution(&ti.type_params, &type_args);
        substitute_preserving(&get_sig.return_type, &subst)
    };

    match &option_ty {
        Type::Named {
            identifier,
            type_args,
        } if identifier.name == "Option" && !type_args.is_empty() => Some(type_args[0].clone()),
        other => Some(other.clone()),
    }
}
