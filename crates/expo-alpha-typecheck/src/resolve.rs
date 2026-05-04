//! Resolve sub-pass: walk every body in `file`, populating `Resolution`
//! on identifier references and `Expr.resolution` (a [`ResolvedType`]
//! pointing into the [`GlobalRegistry`]) on every expression.
//!
//! The POC scope covers integer arithmetic, the boolean operators
//! (`and`, `or`, `not`), and the comparison operators
//! (`== != < > <= >=`). Identifier references and richer shapes land
//! when a future `lift_signatures` pass starts publishing resolved
//! signatures the resolver can look up.
//!
//! Type identity is registry-backed: `Int`, `Bool`, `Unit` come from
//! the stdlib struct stubs preloaded by [`GlobalRegistry::with_stdlib_stubs`].
//! The resolver never caches ids -- every primitive production goes
//! through `registry.lookup(&Identifier)` so the registry stays the
//! single source of truth for what `Int` means.

use expo_ast::ast::{
    BinOp, Diagnostic, Expr, ExprKind, File, Function, Item, Literal, Statement, UnaryOp,
};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

use crate::labels::expr_kind_label;
use crate::registry::GlobalRegistry;

pub(crate) fn resolve_file(
    file: &mut File,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for item in &mut file.items {
        if let Item::Function(function) = item {
            resolve_function(function, registry, diagnostics);
        }
    }
}

fn resolve_function(
    function: &mut Function,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(body) = function.body.as_mut() else {
        return;
    };
    for stmt in body.iter_mut() {
        resolve_statement(stmt, registry, diagnostics);
    }
}

fn resolve_statement(
    stmt: &mut Statement,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match stmt {
        Statement::Assignment { value, .. } | Statement::CompoundAssign { value, .. } => {
            resolve_expr(value, registry, diagnostics);
        }
        Statement::Break { .. } => {}
        Statement::Expr(expr) => {
            resolve_expr(expr, registry, diagnostics);
        }
        Statement::Return { value, .. } => {
            if let Some(value) = value {
                resolve_expr(value, registry, diagnostics);
            }
        }
    }
}

fn resolve_expr(expr: &mut Expr, registry: &GlobalRegistry, diagnostics: &mut Vec<Diagnostic>) {
    let ty = match &mut expr.kind {
        ExprKind::Binary { op, left, right } => {
            resolve_expr(left, registry, diagnostics);
            resolve_expr(right, registry, diagnostics);
            binary_type(*op, left, right, expr.span, registry, diagnostics)
        }
        ExprKind::Unary { op, operand } => {
            resolve_expr(operand, registry, diagnostics);
            unary_type(*op, operand, expr.span, registry, diagnostics)
        }
        ExprKind::Group { expr: inner } => {
            resolve_expr(inner, registry, diagnostics);
            inner.resolution.clone()
        }
        ExprKind::Literal { value } => literal_type(value, registry),
        // Anything else: emit a diagnostic and leave the expression
        // unresolved. The POC does not need to support these shapes;
        // they unblock as features land. Seal runs only on the success
        // path, so an `Unresolved` leaf here is fine -- diagnostics is
        // non-empty and `check_program` will return early.
        other => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck POC does not yet support expression `{}`",
                    expr_kind_label(other)
                ),
                expr.span,
            ));
            ResolvedType::unresolved()
        }
    };
    expr.resolution = ty;
}

fn literal_type(value: &Literal, registry: &GlobalRegistry) -> ResolvedType {
    match value {
        Literal::Bool(_) => primitive(registry, "Bool"),
        Literal::Float(_) => primitive(registry, "Float"),
        Literal::Int(_) => primitive(registry, "Int"),
        Literal::String(_) => primitive(registry, "String"),
        Literal::Unit => primitive(registry, "Unit"),
    }
}

fn binary_type(
    op: BinOp,
    left: &Expr,
    right: &Expr,
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let lhs = &left.resolution;
    let rhs = &right.resolution;
    match op {
        BinOp::Add | BinOp::Div | BinOp::Mod | BinOp::Mul | BinOp::Sub => {
            if is_primitive(lhs, registry, "Int") && is_primitive(rhs, registry, "Int") {
                primitive(registry, "Int")
            } else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "alpha typecheck POC supports integer arithmetic only; got `{}` and `{}`",
                        display_resolution(lhs, registry),
                        display_resolution(rhs, registry),
                    ),
                    span,
                ));
                ResolvedType::unresolved()
            }
        }
        BinOp::And | BinOp::Or => {
            if is_primitive(lhs, registry, "Bool") && is_primitive(rhs, registry, "Bool") {
                primitive(registry, "Bool")
            } else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "`{}` requires Bool operands; got `{}` and `{}`",
                        bin_op_label(op),
                        display_resolution(lhs, registry),
                        display_resolution(rhs, registry),
                    ),
                    span,
                ));
                ResolvedType::unresolved()
            }
        }
        BinOp::Eq | BinOp::NotEq => {
            let both_int = is_primitive(lhs, registry, "Int") && is_primitive(rhs, registry, "Int");
            let both_bool =
                is_primitive(lhs, registry, "Bool") && is_primitive(rhs, registry, "Bool");
            if both_int || both_bool {
                primitive(registry, "Bool")
            } else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "`{}` requires matching Int or Bool operands; got `{}` and `{}`",
                        bin_op_label(op),
                        display_resolution(lhs, registry),
                        display_resolution(rhs, registry),
                    ),
                    span,
                ));
                ResolvedType::unresolved()
            }
        }
        BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq => {
            if is_primitive(lhs, registry, "Int") && is_primitive(rhs, registry, "Int") {
                primitive(registry, "Bool")
            } else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "`{}` requires Int operands; got `{}` and `{}`",
                        bin_op_label(op),
                        display_resolution(lhs, registry),
                        display_resolution(rhs, registry),
                    ),
                    span,
                ));
                ResolvedType::unresolved()
            }
        }
        _ => {
            diagnostics.push(Diagnostic::error(
                format!("alpha typecheck POC does not yet support binary operator `{op:?}`"),
                span,
            ));
            ResolvedType::unresolved()
        }
    }
}

fn unary_type(
    op: UnaryOp,
    operand: &Expr,
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let ty = &operand.resolution;
    match op {
        UnaryOp::Not => {
            if is_primitive(ty, registry, "Bool") {
                primitive(registry, "Bool")
            } else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "`not` requires a Bool operand; got `{}`",
                        display_resolution(ty, registry),
                    ),
                    span,
                ));
                ResolvedType::unresolved()
            }
        }
        UnaryOp::Neg => {
            if is_primitive(ty, registry, "Int") {
                primitive(registry, "Int")
            } else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "unary `-` requires an Int operand; got `{}`",
                        display_resolution(ty, registry),
                    ),
                    span,
                ));
                ResolvedType::unresolved()
            }
        }
    }
}

/// Build a leaf `ResolvedType` for a preloaded `Global.<name>` stdlib
/// struct stub. Panics if the stub is missing -- preload is a
/// [`GlobalRegistry::with_stdlib_stubs`] invariant and a missing stub
/// means the pipeline was wired up wrong.
fn primitive(registry: &GlobalRegistry, name: &str) -> ResolvedType {
    let ident = Identifier::new("Global", vec![name.to_string()]);
    let (id, _) = registry.lookup(&ident).unwrap_or_else(|| {
        panic!(
            "stdlib stub `Global.{name}` missing from registry — \
             alpha pipeline must seed it via `GlobalRegistry::with_stdlib_stubs`",
        )
    });
    ResolvedType::leaf(Resolution::Global(id))
}

/// Tell-don't-ask predicate: does `ty` resolve to the preloaded
/// `Global.<name>` stdlib stub? Guard-clauses short-circuit when any
/// of the required conditions fails, so callers just read the final
/// result.
fn is_primitive(ty: &ResolvedType, registry: &GlobalRegistry, name: &str) -> bool {
    let Resolution::Global(id) = ty.resolution else {
        return false;
    };
    if !ty.type_args.is_empty() {
        return false;
    }
    let Some(entry) = registry.get(id) else {
        return false;
    };
    entry.identifier.is_in_global() && entry.identifier.last() == name
}

/// Human-readable rendering of a `ResolvedType` for diagnostics.
/// Dereferences the head through the registry when it's `Global` so
/// users see `Int` rather than an opaque `#0`.
fn display_resolution(ty: &ResolvedType, registry: &GlobalRegistry) -> String {
    match ty.resolution {
        Resolution::Unresolved => "<unresolved>".to_string(),
        Resolution::Global(id) => match registry.get(id) {
            Some(entry) => entry.identifier.last().to_string(),
            None => format!("<id {id}>"),
        },
    }
}

fn bin_op_label(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::And => "and",
        BinOp::Concat => "<>",
        BinOp::Div => "/",
        BinOp::Eq => "==",
        BinOp::Gt => ">",
        BinOp::GtEq => ">=",
        BinOp::Lt => "<",
        BinOp::LtEq => "<=",
        BinOp::Mod => "%",
        BinOp::Mul => "*",
        BinOp::NotEq => "!=",
        BinOp::Or => "or",
        BinOp::Sub => "-",
    }
}
