//! Resolve sub-pass: walk every body, populating `Resolution` on
//! identifier references and `Expr.resolution` on every expression.
//!
//! Today's scope covers integer arithmetic, boolean (`and`/`or`/`not`),
//! comparison (`== != < > <= >=`), and bare-identifier function calls.
//! Local references (including parameter uses) land with
//! [`Resolution::Local`] in a follow-up slice.
//!
//! Type identity is registry-backed: every primitive production goes
//! through `registry.lookup(&Identifier)` so the registry stays the
//! single source of truth for what `Int` (etc.) means.
//!
//! # Call resolution
//!
//! Calls accept only bare-`Ident` callees. The inner `Ident.resolution`
//! is stamped with the callee's `GlobalRegistryId`; the outer callee
//! `Expr.resolution` stays `Unresolved` (seal carves this out) because
//! function names aren't first-class values yet. The call-site
//! `Expr.resolution` takes the callee's return type.

use expo_ast::ast::{
    Arg, BinOp, Diagnostic, Expr, ExprKind, File, Function, Item, Literal, Statement, UnaryOp,
};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

use crate::labels::expr_kind_label;
use crate::registry::{GlobalKind, GlobalRegistry};

pub(crate) fn resolve_file(
    file: &mut File,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for item in &mut file.items {
        if let Item::Function(function) = item {
            resolve_function(function, package, registry, diagnostics);
        }
    }
    if let Some(body) = file.body.as_mut() {
        for stmt in body.iter_mut() {
            resolve_statement(stmt, package, registry, diagnostics);
        }
    }
}

fn resolve_function(
    function: &mut Function,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(body) = function.body.as_mut() else {
        return;
    };
    for stmt in body.iter_mut() {
        resolve_statement(stmt, package, registry, diagnostics);
    }
}

fn resolve_statement(
    stmt: &mut Statement,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match stmt {
        Statement::Assignment { value, .. } | Statement::CompoundAssign { value, .. } => {
            resolve_expr(value, package, registry, diagnostics);
        }
        Statement::Break { .. } => {}
        Statement::Expr(expr) => {
            resolve_expr(expr, package, registry, diagnostics);
        }
        Statement::Return { value, .. } => {
            if let Some(value) = value {
                resolve_expr(value, package, registry, diagnostics);
            }
        }
    }
}

fn resolve_expr(
    expr: &mut Expr,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let ty = match &mut expr.kind {
        ExprKind::Binary { op, left, right } => {
            resolve_expr(left, package, registry, diagnostics);
            resolve_expr(right, package, registry, diagnostics);
            binary_type(*op, left, right, expr.span, registry, diagnostics)
        }
        ExprKind::Unary { op, operand } => {
            resolve_expr(operand, package, registry, diagnostics);
            unary_type(*op, operand, expr.span, registry, diagnostics)
        }
        ExprKind::Group { expr: inner } => {
            resolve_expr(inner, package, registry, diagnostics);
            inner.resolution.clone()
        }
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => resolve_if(
            condition,
            then_body,
            else_body.as_deref_mut(),
            expr.span,
            package,
            registry,
            diagnostics,
        ),
        ExprKind::Unless { condition, body } => {
            resolve_unless(condition, body, expr.span, package, registry, diagnostics)
        }
        ExprKind::Literal { value } => literal_type(value, registry),
        ExprKind::Call { callee, args } => {
            resolve_call(callee, args, expr.span, package, registry, diagnostics)
        }
        ExprKind::Ident { name, .. } => {
            // Local references (including parameter uses) are not yet
            // supported. `Resolution::Local` lands with the follow-up
            // slice; until then emit a dedicated diagnostic.
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck does not yet support identifier references in function \
                     bodies (got `{name}`)",
                ),
                expr.span,
            ));
            ResolvedType::unresolved()
        }
        // Unsupported shapes diagnose and leave the expression
        // unresolved. Seal runs only on the success path, so an
        // `Unresolved` leaf here is harmless — diagnostics is non-empty
        // and `check_program` returns early.
        other => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck does not yet support expression `{}`",
                    expr_kind_label(other)
                ),
                expr.span,
            ));
            ResolvedType::unresolved()
        }
    };
    expr.resolution = ty;
}

fn resolve_call(
    callee: &mut Expr,
    args: &mut [Arg],
    call_span: Span,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    // Resolve arguments first regardless of whether the callee is
    // well-formed, so nested errors surface and `seal_expr` has
    // resolutions to walk on each arg.
    for arg in args.iter_mut() {
        if let Some(name) = arg.name.as_ref() {
            diagnostics.push(Diagnostic::error(
                format!("alpha typecheck does not yet support named arguments (got `{name}`)",),
                arg.span,
            ));
        }
        resolve_expr(&mut arg.value, package, registry, diagnostics);
    }

    let ExprKind::Ident {
        name,
        resolution: ident_resolution,
    } = &mut callee.kind
    else {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck only supports bare-identifier callees (got `{}`)",
                expr_kind_label(&callee.kind),
            ),
            callee.span,
        ));
        return ResolvedType::unresolved();
    };

    let candidate = Identifier::new(package, vec![name.clone()]);
    let Some((id, entry)) = registry.lookup(&candidate) else {
        diagnostics.push(Diagnostic::error(
            format!("unknown function `{name}`"),
            callee.span,
        ));
        return ResolvedType::unresolved();
    };

    let sig = match &entry.kind {
        GlobalKind::Function(Some(sig)) => sig,
        GlobalKind::Function(None) => panic!(
            "resolve_call: function `{}` has no lifted signature — \
             lift_signatures must run before resolve",
            entry.identifier,
        ),
        other => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "cannot call `{name}`: it is a {}, not a function",
                    other.label(),
                ),
                callee.span,
            ));
            return ResolvedType::unresolved();
        }
    };

    *ident_resolution = Resolution::Global(id);

    let return_type = sig.return_type.clone();

    if args.len() != sig.params.len() {
        diagnostics.push(Diagnostic::error(
            format!(
                "`{}` expects {} argument{}, got {}",
                entry.identifier,
                sig.params.len(),
                if sig.params.len() == 1 { "" } else { "s" },
                args.len(),
            ),
            call_span,
        ));
        return return_type;
    }

    for (arg, param) in args.iter().zip(sig.params.iter()) {
        let actual = &arg.value.resolution;
        if !actual.is_resolved() {
            // Arg already triggered its own diagnostic; skip the
            // follow-up to avoid noise.
            continue;
        }
        if actual != &param.ty {
            diagnostics.push(Diagnostic::error(
                format!(
                    "argument `{}` to `{}` expects `{}`, got `{}`",
                    param.name,
                    entry.identifier,
                    display_resolution(&param.ty, registry),
                    display_resolution(actual, registry),
                ),
                arg.span,
            ));
        }
    }

    return_type
}

/// Resolve `if cond do then_body end` (no `else` in this slice).
/// Restricts the condition to `Bool`, recursively resolves the body
/// statements, and types the whole expression as `Unit`. `else`
/// branches surface as a feature-gap diagnostic — value-producing
/// `if` / `else` lands with the locals slice once alloca-style
/// result slots are available.
fn resolve_if(
    condition: &mut Expr,
    then_body: &mut [Statement],
    else_body: Option<&mut [Statement]>,
    span: Span,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_expr(condition, package, registry, diagnostics);
    require_bool_condition("if", condition, registry, diagnostics);
    for stmt in then_body.iter_mut() {
        resolve_statement(stmt, package, registry, diagnostics);
    }
    if let Some(else_body) = else_body {
        diagnostics.push(Diagnostic::error(
            "alpha typecheck does not yet support `else` branches",
            span,
        ));
        for stmt in else_body.iter_mut() {
            resolve_statement(stmt, package, registry, diagnostics);
        }
    }
    primitive(registry, "Unit")
}

/// Resolve `unless cond do body end`. Same shape as `if` minus the
/// `else` carve-out (the parser doesn't admit `else` on `unless`),
/// always Unit-typed.
fn resolve_unless(
    condition: &mut Expr,
    body: &mut [Statement],
    _span: Span,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_expr(condition, package, registry, diagnostics);
    require_bool_condition("unless", condition, registry, diagnostics);
    for stmt in body.iter_mut() {
        resolve_statement(stmt, package, registry, diagnostics);
    }
    primitive(registry, "Unit")
}

/// Diagnose a non-Bool condition on an `if` / `unless`. Skips the
/// check when the condition itself failed to resolve — its own
/// diagnostic is already in flight.
fn require_bool_condition(
    keyword: &str,
    condition: &Expr,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !condition.resolution.is_resolved() {
        return;
    }
    if !is_primitive(&condition.resolution, registry, "Bool") {
        diagnostics.push(Diagnostic::error(
            format!(
                "`{keyword}` condition must be `Bool`, got `{}`",
                display_resolution(&condition.resolution, registry),
            ),
            condition.span,
        ));
    }
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
                        "alpha typecheck supports integer arithmetic only; got `{}` and `{}`",
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
                format!("alpha typecheck does not yet support binary operator `{op:?}`"),
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
/// struct stub. Panics if the stub is missing — preload is a
/// [`GlobalRegistry::with_stdlib_stubs`] invariant.
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

/// Does `ty` resolve to the preloaded `Global.<name>` stdlib stub?
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

/// Human-readable rendering of a `ResolvedType` for diagnostics:
/// dereferences `Global` heads through the registry so users see
/// `Int` rather than an opaque `#0`.
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
