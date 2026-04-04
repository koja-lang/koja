//! Module and function-level type checking entry points.
//!
//! Contains [`check_module`], the public entry point that walks all function
//! bodies and impl blocks, plus shared helper functions used across the
//! type-checking modules.

use std::collections::{BTreeMap, HashMap, HashSet};

use expo_ast::ast::*;
use expo_ast::span::Span;

use crate::context::{Coercion, FunctionKind, ParamInfo, PassMode, TypeContext};
use crate::env::{CheckEnv, VarInfo, VarState};
use crate::expr::{expr_span, infer_expr, infer_expr_with_expected};
use crate::stmt::check_body;
use crate::types::numeric_compatible;
use crate::types::{Primitive, Type, resolve_type_expr_with_params, substitute_preserving};

/// Type-checks all function bodies and impl blocks in a module, emitting
/// diagnostics for type mismatches, undefined variables, and exhaustiveness errors.
pub fn check_module(module: &Module, ctx: &mut TypeContext) {
    let struct_names = ctx.struct_names();
    let struct_name_refs: Vec<&str> = struct_names.iter().map(|s| s.as_str()).collect();
    let enum_names = ctx.enum_names();
    let enum_name_refs: Vec<&str> = enum_names.iter().map(|s| s.as_str()).collect();

    for item in &module.items {
        match item {
            Item::Function(f) => {
                if f.type_params.is_empty() {
                    check_function(f, ctx, None, &struct_name_refs, &enum_name_refs);
                }
            }
            Item::Impl(impl_block) => {
                let (target_name, is_generic_impl) = match &impl_block.target {
                    TypeExpr::Named { path, .. } if path.len() == 1 => (&path[0], false),
                    TypeExpr::Generic { path, .. } if path.len() == 1 => (&path[0], true),
                    _ => continue,
                };
                if is_generic_impl {
                    continue;
                }
                let self_type = if ctx.is_struct(target_name) {
                    Type::Struct(target_name.clone())
                } else if ctx.is_enum(target_name) {
                    Type::Enum(target_name.clone())
                } else if let Some(p) = Primitive::from_name(target_name) {
                    Type::Primitive(p)
                } else {
                    continue;
                };

                let impl_process_msg =
                    ctx.protocol_impls
                        .get(target_name.as_str())
                        .and_then(|impls| {
                            impls.iter().find(|(proto, _)| proto == "Process").and_then(
                                |(_, args)| {
                                    let m = args.get(1)?;
                                    let r = args.get(2)?;
                                    Some(crate::types::process_envelope_type(m, r))
                                },
                            )
                        });

                for member in &impl_block.members {
                    if let ImplMember::Function(f) = member
                        && f.type_params.is_empty()
                    {
                        check_function_with_msg(
                            f,
                            ctx,
                            Some(&self_type),
                            &struct_name_refs,
                            &enum_name_refs,
                            impl_process_msg.clone(),
                        );
                    }
                }
                let synth_fns = ctx
                    .synthesized_default_fns
                    .get(target_name.as_str())
                    .cloned()
                    .unwrap_or_default();
                for f in &synth_fns {
                    if f.type_params.is_empty() {
                        check_function_with_msg(
                            f,
                            ctx,
                            Some(&self_type),
                            &struct_name_refs,
                            &enum_name_refs,
                            impl_process_msg.clone(),
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

/// Type-checks a single function body using the default message type.
fn check_function(
    f: &Function,
    ctx: &mut TypeContext,
    self_type: Option<&Type>,
    struct_names: &[&str],
    enum_names: &[&str],
) {
    check_function_with_msg(f, ctx, self_type, struct_names, enum_names, None);
}

/// Type-checks a function body, building a [`CheckEnv`] from its parameters
/// and verifying the return type against the declared signature. When
/// `override_msg_type` is `Some`, it replaces the process mailbox type.
fn check_function_with_msg(
    f: &Function,
    ctx: &mut TypeContext,
    self_type: Option<&Type>,
    struct_names: &[&str],
    enum_names: &[&str],
    override_msg_type: Option<Type>,
) {
    let self_subst: Option<HashMap<String, Type>> =
        self_type.map(|ty| HashMap::from([("Self".to_string(), ty.clone())]));
    let self_params: &[&str] = if self_type.is_some() { &["Self"] } else { &[] };

    let resolve = |te: &TypeExpr| -> Type {
        let ty = resolve_type_expr_with_params(
            te,
            struct_names,
            enum_names,
            self_params,
            &BTreeMap::new(),
        );
        match &self_subst {
            Some(subst) => substitute_preserving(&ty, subst),
            None => ty,
        }
    };

    let mut env: HashMap<String, VarInfo> = HashMap::new();

    if let Some(ty) = self_type {
        env.insert(
            "self".to_string(),
            VarInfo {
                ty: ty.clone(),
                state: VarState::Live,
            },
        );
    }

    for param in &f.params {
        if let Param::Regular {
            name, type_expr, ..
        } = param
        {
            let ty = resolve(type_expr);
            env.insert(
                name.clone(),
                VarInfo {
                    ty,
                    state: VarState::Live,
                },
            );
        }
    }

    let declared_return = f.return_type.as_ref().map(&resolve).unwrap_or(Type::Unit);

    if f.body.is_empty() {
        return;
    }

    let kind = f
        .params
        .iter()
        .find_map(|p| match p {
            Param::Self_ { mode, .. } => Some(FunctionKind::Instance(*mode)),
            _ => None,
        })
        .unwrap_or(FunctionKind::Static);

    let process_msg_type = override_msg_type;

    let mut ce = CheckEnv {
        env,
        used_vars: HashSet::new(),
        loop_depth: 0,
        return_type: declared_return.clone(),
        kind,
        struct_names,
        enum_names,
        type_hint: None,
        process_msg_type,
        fn_type_params: f.type_params.clone(),
    };

    let check_implicit_return = declared_return != Type::Unit && declared_return != Type::Unknown;
    let last_is_expr = matches!(f.body.last(), Some(Statement::Expr(_)));

    if check_implicit_return && last_is_expr {
        check_body(&f.body[..f.body.len() - 1], ctx, &mut ce);
        if let Some(Statement::Expr(expr)) = f.body.last() {
            let actual = infer_expr(expr, ctx, &mut ce);
            if actual.is_known()
                && !types_compatible(&actual, &declared_return)
                && !is_diverging(expr)
            {
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
            } else if actual.is_known() {
                record_coercion_if_needed(&actual, &declared_return, expr_span(expr), ctx);
            }
        }
    } else {
        check_body(&f.body, ctx, &mut ce);
    }
}

/// Validates that call arguments match the expected parameter count and types,
/// emitting diagnostics for arity mismatches or type mismatches.
pub(crate) fn check_call_args(
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
            let param = &params[i];
            let arg_ty = infer_expr_with_expected(&arg.value, Some(&param.ty), ctx, ce);
            if param.ty.is_known() && arg_ty.is_known() {
                if !types_compatible(&arg_ty, &param.ty) {
                    ctx.error(
                        format!(
                            "argument `{}`: expected `{}`, found `{}`",
                            param.name,
                            param.ty.display(),
                            arg_ty.display()
                        ),
                        arg.span,
                    );
                } else {
                    record_coercion_if_needed(&arg_ty, &param.ty, arg.span, ctx);
                }
            }
            if param.mode == PassMode::Move
                && !arg_ty.is_copy()
                && let Expr::Ident { name, .. } = &arg.value
            {
                ce.mark_moved(name, arg.span);
            }
        }
    }
}

/// Compares actual vs expected type and reports a diagnostic on mismatch.
pub(crate) fn check_type(actual: &Type, expected: &Type, span: Span, ctx: &mut TypeContext) {
    if !actual.is_known() || !expected.is_known() {
        return;
    }
    if !types_compatible(actual, expected) {
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

/// Attempts to parse a mangled generic name (e.g. `Pair_$i32.i32$`) back into
/// the base name and concrete type arguments for method resolution.
pub(crate) fn try_parse_mangled_generic(
    name: &str,
    ctx: &TypeContext,
) -> Option<(String, Vec<Type>)> {
    let sep_pos = name.find("_$")?;
    let base = &name[..sep_pos];
    if !ctx.types.contains_key(base) {
        return None;
    }
    if !name.ends_with('$') {
        return None;
    }
    let inner = &name[sep_pos + 2..name.len() - 1];
    let parts = split_mangled_args(inner);
    let type_args: Vec<Type> = parts
        .iter()
        .map(|s| {
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

/// Splits a mangled args string on `.` at depth 0, respecting nested `_$...$`.
fn split_mangled_args(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'_' && bytes[i + 1] == b'$' {
            depth += 1;
            current.push('_');
            current.push('$');
            i += 2;
        } else if bytes[i] == b'$' {
            depth -= 1;
            current.push('$');
            i += 1;
        } else if bytes[i] == b'.' && depth == 0 {
            parts.push(std::mem::take(&mut current));
            i += 1;
        } else {
            current.push(bytes[i] as char);
            i += 1;
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Returns `true` when `expr` is a call to a diverging function (e.g. `panic`)
/// whose return type should be treated as compatible with any declared type.
fn is_diverging(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Call { callee, .. }
            if matches!(callee.as_ref(), Expr::Ident { name, .. } if name == "panic")
    )
}

/// Checks if two types are compatible, accounting for numeric coercion and
/// generic instances with partially-known type arguments.
pub(crate) fn types_compatible(a: &Type, b: &Type) -> bool {
    let a = match a {
        Type::Indirect(inner) => inner.as_ref(),
        other => other,
    };
    let b = match b {
        Type::Indirect(inner) => inner.as_ref(),
        other => other,
    };
    if a == b || numeric_compatible(a, b) {
        return true;
    }
    // A concrete type is compatible with a union if it's one of the constituents (widening)
    if let Type::Union(members) = b {
        return members.iter().any(|m| types_compatible(a, m));
    }
    // Two unions are compatible if they have the same canonical members
    if let (Type::Union(ma), Type::Union(mb)) = (a, b) {
        return ma == mb;
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

/// If `target` is a union and `source` is a non-union constituent, records a
/// widening coercion so the codegen can emit the tag+payload wrapper.
pub(crate) fn record_coercion_if_needed(
    source: &Type,
    target: &Type,
    span: Span,
    ctx: &mut TypeContext,
) {
    if let Type::Union(members) = target
        && !matches!(source, Type::Union(_))
        && members.iter().any(|m| types_compatible(source, m))
    {
        ctx.coercions.insert(
            span,
            Coercion::UnionWiden {
                source: source.clone(),
                target: target.clone(),
            },
        );
    }
}

/// Checks whether a literal integer value (possibly negated) fits in the given
/// bit width, emitting a diagnostic on overflow.
pub(crate) fn check_literal_overflow(
    value_expr: &Expr,
    bits: u64,
    signedness: Option<BinarySignedness>,
    span: Span,
    ctx: &mut TypeContext,
) {
    if bits == 0 || bits > 64 {
        return;
    }

    let val = match value_expr {
        Expr::Literal {
            value: Literal::Int(n),
            ..
        } => n.parse::<i128>().ok(),
        Expr::Unary {
            op: UnaryOp::Neg,
            operand,
            ..
        } => {
            if let Expr::Literal {
                value: Literal::Int(n),
                ..
            } = operand.as_ref()
            {
                n.parse::<i128>().ok().map(|v| -v)
            } else {
                None
            }
        }
        _ => None,
    };

    let Some(val) = val else { return };

    let is_signed = signedness == Some(BinarySignedness::Signed);
    if is_signed {
        let min = -(1i128 << (bits - 1));
        let max = (1i128 << (bits - 1)) - 1;
        if val < min || val > max {
            ctx.error(
                format!("{val} does not fit in {bits} signed bits (range {min}..{max})"),
                span,
            );
        }
    } else {
        let max = if bits >= 128 {
            i128::MAX
        } else {
            (1i128 << bits) - 1
        };
        if val < 0 || val > max {
            ctx.error(
                format!("{val} does not fit in {bits} unsigned bits (range 0..{max})"),
                span,
            );
        }
    }
}
