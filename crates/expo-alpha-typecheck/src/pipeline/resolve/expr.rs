//! Expression dispatch: pattern-matches `ExprKind` and routes to the
//! per-shape resolver in [`super::control_flow`] (if/unless),
//! [`super::ops`] (literal/binary/unary), or this module (calls,
//! groups, idents). Every successful arm returns the
//! [`ResolvedType`] to stamp on `expr.resolution`.
//!
//! # Call resolution
//!
//! Calls accept only bare-`Ident` callees. The inner `Ident.resolution`
//! is stamped with the callee's [`GlobalRegistryId`]; the outer callee
//! `Expr.resolution` stays `Unresolved` (seal carves this out) because
//! function names aren't first-class values yet. The call-site
//! `Expr.resolution` takes the callee's return type.
//!
//! [`GlobalRegistryId`]: expo_ast::identifier::GlobalRegistryId

use expo_ast::ast::{Arg, Diagnostic, Expr, ExprKind};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

use crate::labels::expr_kind_label;
use crate::registry::{GlobalKind, GlobalRegistry};

use super::control_flow::{resolve_if, resolve_unless};
use super::ops::{binary_type, literal_type, unary_type};
use super::types::display_resolution;

pub(super) fn resolve_expr(
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
        ExprKind::Call { callee, args } => {
            resolve_call(callee, args, expr.span, package, registry, diagnostics)
        }
        ExprKind::Group { expr: inner } => {
            resolve_expr(inner, package, registry, diagnostics);
            inner.resolution.clone()
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
        ExprKind::Literal { value } => literal_type(value, registry),
        ExprKind::Unary { op, operand } => {
            resolve_expr(operand, package, registry, diagnostics);
            unary_type(*op, operand, expr.span, registry, diagnostics)
        }
        ExprKind::Unless { condition, body } => {
            resolve_unless(condition, body, package, registry, diagnostics)
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
