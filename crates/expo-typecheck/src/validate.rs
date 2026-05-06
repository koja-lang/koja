//! Post-typecheck validation: ensures no `Package::Unresolved` identifiers
//! remain in `expr.resolved_type` before the AST reaches codegen.

use expo_ast::ast::{
    BinarySegment, CondArm, EnumConstructionData, Expr, ExprKind, FieldInit, File, Function, Item,
    MatchArm, Statement, StringPart,
};
use expo_ast::identifier::{Package, TypeIdentifier};
use expo_ast::types::Type;

use crate::context::TypeContext;

/// Walks every expression in a file and emits an error for any
/// `expr.resolved_type` that contains a `Package::Unresolved` identifier.
///
/// Call after `check_file` to enforce the invariant that all types reaching
/// codegen are fully package-resolved.
pub fn validate_resolved_types(file: &File, ctx: &mut TypeContext) {
    for item in &file.items {
        validate_item(item, ctx);
    }
}

fn validate_item(item: &Item, ctx: &mut TypeContext) {
    match item {
        Item::Constant(c) => validate_expr(&c.value, ctx),
        Item::Enum(e) => {
            for f in &e.functions {
                validate_function(f, ctx);
            }
            for v in &e.variants {
                if let expo_ast::ast::EnumVariantData::Struct(fields) = &v.data {
                    for field in fields {
                        if let Some(ref default) = field.default {
                            validate_expr(default, ctx);
                        }
                    }
                }
            }
        }
        Item::Function(f) => validate_function(f, ctx),
        Item::Impl(block) => {
            for member in &block.members {
                if let expo_ast::ast::ImplMember::Function(f) = member {
                    validate_function(f, ctx);
                }
            }
        }
        Item::Struct(s) => {
            for field in &s.fields {
                if let Some(ref default) = field.default {
                    validate_expr(default, ctx);
                }
            }
            for f in &s.functions {
                validate_function(f, ctx);
            }
        }
        Item::Alias(_) | Item::Protocol(_) | Item::TypeAlias(_) => {}
    }
}

fn validate_function(f: &Function, ctx: &mut TypeContext) {
    if let Some(body) = &f.body {
        validate_body(body, ctx);
    }
}

fn validate_body(stmts: &[Statement], ctx: &mut TypeContext) {
    for stmt in stmts {
        validate_statement(stmt, ctx);
    }
}

fn validate_statement(stmt: &Statement, ctx: &mut TypeContext) {
    match stmt {
        Statement::Assignment { value, .. } => validate_expr(value, ctx),
        Statement::CompoundAssign { value, .. } => validate_expr(value, ctx),
        Statement::Expr(expr) => validate_expr(expr, ctx),
        Statement::Return { value, .. } => {
            if let Some(expr) = value {
                validate_expr(expr, ctx);
            }
        }
        Statement::Break { .. } => {}
    }
}

fn validate_expr(expr: &Expr, ctx: &mut TypeContext) {
    if let Some(ref ty) = expr.resolved_type {
        check_type_resolved(ty, expr, ctx);
    }

    match &expr.kind {
        ExprKind::Closure { body, .. } | ExprKind::Loop { body } => {
            validate_body(body, ctx);
        }
        ExprKind::For { iterable, body, .. } => {
            validate_expr(iterable, ctx);
            validate_body(body, ctx);
        }
        ExprKind::Unless { condition, body } | ExprKind::While { condition, body } => {
            validate_expr(condition, ctx);
            validate_body(body, ctx);
        }

        ExprKind::Binary { left, right, .. } => {
            validate_expr(left, ctx);
            validate_expr(right, ctx);
        }
        ExprKind::BinaryLiteral { segments } => validate_binary_segments(segments, ctx),
        ExprKind::Call { callee, args } => {
            validate_expr(callee, ctx);
            validate_args(args, ctx);
        }
        ExprKind::Cond { arms, else_body } => {
            validate_cond_arms(arms, ctx);
            if let Some(body) = else_body {
                validate_body(body, ctx);
            }
        }
        ExprKind::EnumConstruction { data, .. } => match data {
            EnumConstructionData::Struct(fields) => validate_field_inits(fields, ctx),
            EnumConstructionData::Tuple(exprs) => {
                for e in exprs {
                    validate_expr(e, ctx);
                }
            }
            EnumConstructionData::Unit => {}
        },
        ExprKind::FieldAccess { receiver, .. } | ExprKind::MethodCall { receiver, .. } => {
            validate_expr(receiver, ctx);
            if let ExprKind::MethodCall { args, .. } = &expr.kind {
                validate_args(args, ctx);
            }
        }
        ExprKind::Group { expr: inner } | ExprKind::Spawn { expr: inner } => {
            validate_expr(inner, ctx);
        }
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            validate_expr(condition, ctx);
            validate_body(then_body, ctx);
            if let Some(body) = else_body {
                validate_body(body, ctx);
            }
        }
        ExprKind::List { elements } => {
            for e in elements {
                validate_expr(e, ctx);
            }
        }
        ExprKind::Map { entries } => {
            for (k, v) in entries {
                validate_expr(k, ctx);
                validate_expr(v, ctx);
            }
        }
        ExprKind::Match { subject, arms } => {
            validate_expr(subject, ctx);
            validate_match_arms(arms, ctx);
        }
        ExprKind::Receive {
            arms,
            after_timeout,
            after_body,
        } => {
            validate_match_arms(arms, ctx);
            if let Some(timeout) = after_timeout {
                validate_expr(timeout, ctx);
            }
            validate_body(after_body, ctx);
        }
        ExprKind::ShortClosure { body, .. } => validate_expr(body, ctx),
        ExprKind::String { parts, .. } => {
            for part in parts {
                if let StringPart::Interpolation { expr, .. } = part {
                    validate_expr(expr, ctx);
                }
            }
        }
        ExprKind::StructConstruction { fields, .. } => validate_field_inits(fields, ctx),
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            validate_expr(condition, ctx);
            validate_expr(then_expr, ctx);
            validate_expr(else_expr, ctx);
        }
        ExprKind::Unary { operand, .. } => validate_expr(operand, ctx),

        ExprKind::Ident { .. } | ExprKind::Literal { .. } | ExprKind::Self_ { .. } => {}
    }
}

fn validate_args(args: &[expo_ast::ast::Arg], ctx: &mut TypeContext) {
    for arg in args {
        validate_expr(&arg.value, ctx);
    }
}

fn validate_binary_segments(segments: &[BinarySegment], ctx: &mut TypeContext) {
    for seg in segments {
        validate_expr(&seg.value, ctx);
        if let Some(size) = &seg.size {
            validate_expr(size, ctx);
        }
    }
}

fn validate_cond_arms(arms: &[CondArm], ctx: &mut TypeContext) {
    for arm in arms {
        validate_expr(&arm.condition, ctx);
        validate_body(&arm.body, ctx);
    }
}

fn validate_field_inits(fields: &[FieldInit], ctx: &mut TypeContext) {
    for fi in fields {
        validate_expr(&fi.value, ctx);
    }
}

fn validate_match_arms(arms: &[MatchArm], ctx: &mut TypeContext) {
    for arm in arms {
        if let Some(guard) = &arm.guard {
            validate_expr(guard, ctx);
        }
        validate_body(&arm.body, ctx);
    }
}

fn check_type_resolved(ty: &Type, expr: &Expr, ctx: &mut TypeContext) {
    match ty {
        Type::Named {
            identifier,
            type_args,
        } => {
            match &identifier.package {
                Package::Unresolved => {
                    let (message, hint) = unresolved_type_diagnostic(&identifier.name, ctx);
                    match hint {
                        Some(h) => ctx.error_with_hint(message, h, expr.span),
                        None => ctx.error(message, expr.span),
                    }
                }
                _ if !ctx.types.contains_key(identifier) => {
                    let (message, hint) = unknown_qualified_diagnostic(identifier, ctx);
                    match hint {
                        Some(h) => ctx.error_with_hint(message, h, expr.span),
                        None => ctx.error(message, expr.span),
                    }
                }
                _ => {}
            }
            for arg in type_args {
                check_type_resolved(arg, expr, ctx);
            }
        }
        Type::Function {
            params,
            return_type,
        } => {
            for p in params {
                check_type_resolved(&p.ty, expr, ctx);
            }
            check_type_resolved(return_type, expr, ctx);
        }
        Type::Indirect(inner) | Type::Pointer(inner) => {
            check_type_resolved(inner, expr, ctx);
        }
        Type::Union(variants) => {
            for v in variants {
                check_type_resolved(v, expr, ctx);
            }
        }
        Type::Error | Type::Parameter(_) | Type::Primitive(_) | Type::Unit | Type::Unknown => {}
    }
}

/// Builds a helpful error message and optional hint for an unresolved type
/// reference. When dependency packages contain a matching type, the hint
/// suggests qualifying the reference (`dep.Name`) or adding an `alias`.
fn unresolved_type_diagnostic(name: &str, ctx: &TypeContext) -> (String, Option<String>) {
    let owners: Vec<String> = ctx
        .package_types
        .iter()
        .filter_map(|(pkg, names)| match pkg {
            Package::Named(p) if names.contains(name) => Some(p.clone()),
            _ => None,
        })
        .collect();
    if owners.is_empty() {
        return (
            format!("unknown type `{name}`: not in current package or `Global`"),
            Some(format!(
                "qualify with the owning package (`pkg.{name}`) or import via `alias pkg.{name}`"
            )),
        );
    }
    let suggestions = owners
        .iter()
        .map(|p| format!("`{p}.{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let alias_hint = owners
        .iter()
        .map(|p| format!("`alias {p}.{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    (
        format!(
            "unknown type `{name}`: bare names resolve to the current package or `Global` only"
        ),
        Some(format!(
            "qualify the reference ({suggestions}) or import via {alias_hint}"
        )),
    )
}

/// Builds an error for a qualified `pkg.Type` reference that names a known
/// package but no such type within it. Reuses
/// [`unresolved_type_diagnostic`] when the bare name is found in some other
/// dependency, since the suggested fix is identical.
fn unknown_qualified_diagnostic(
    identifier: &TypeIdentifier,
    ctx: &TypeContext,
) -> (String, Option<String>) {
    let qualified = identifier.qualified_name();
    let (_, hint) = unresolved_type_diagnostic(&identifier.name, ctx);
    (format!("unknown type `{qualified}`"), hint)
}
