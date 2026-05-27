//! Trailing-expression-vs-declared-return checking.
//!
//! Once `resolve_function` has walked the body, every `Statement::Expr`
//! carries a resolved type. The compiler's contract is that the body's
//! final expression is the function's return value. [`check_return_type`]
//! enforces the typecheck side: when the declared return type is
//! non-Unit, the trailing statement must be a [`Statement::Expr`] whose
//! resolution equals the declared return type.
//!
//! Mirrors v1's `check_implicit_return` ([`koja-typecheck/src/check.rs`])
//! so users see the same diagnostic shape ("return type mismatch:
//! expected `X`, found `Y`") on both pipelines.

use koja_ast::ast::{Diagnostic, Function, Statement};
use koja_ast::coercion::{Coercion, LiteralCoercion};

use crate::registry::FunctionSignature;

use super::coercion::{Compatible, check_compatible, coercion_annotation_mut, coercion_target_mut};
use super::ctx::ResolverEnv;
use super::types::{display_resolution, is_primitive};

/// Diagnose any mismatch between the function's declared return type
/// and the type produced by its trailing expression.
///
/// Skips the check when:
/// - The declared return is `Unit`. The body's last value is discarded
///   and the function returns `()`; arbitrary trailing types are fine.
/// - The declared return is `<unresolved>`. The annotation already
///   triggered its own diagnostic upstream; piling on with a return
///   mismatch only adds noise.
/// - Body is `None` (extern / intrinsic). Those declarations aren't
///   typechecked here.
pub(super) fn check_return_type(
    function: &mut Function,
    signature: &FunctionSignature,
    env: &mut ResolverEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(body) = function.body.as_mut() else {
        return;
    };
    let declared = &signature.return_type;
    if !declared.is_resolved() || is_primitive(declared, env.registry, "Unit") {
        return;
    }
    let Some(last) = body.last_mut() else {
        diagnostics.push(Diagnostic::error(
            format!(
                "return type mismatch on `{}`: expected `{}`, found empty body",
                function.name,
                display_resolution(declared, env.registry),
            ),
            function.span,
        ));
        return;
    };
    let last_span = statement_span(last);
    let Statement::Expr(trailing) = last else {
        diagnostics.push(Diagnostic::error(
            format!(
                "return type mismatch on `{}`: expected `{}`, found a non-expression \
                 trailing statement",
                function.name,
                display_resolution(declared, env.registry),
            ),
            last_span,
        ));
        return;
    };
    let actual = trailing.resolution.clone();
    if !actual.is_resolved() {
        // Trailing expression already triggered its own diagnostic;
        // skip to avoid pile-on noise.
        return;
    }
    // `Never` is the lattice bottom: a body that diverges (e.g. its
    // trailing expression is `if cond then return 1 else return 2 end`,
    // or once `Kernel.panic` lands, a bare `panic()` call) satisfies
    // any non-`Never` declared return type without ever actually
    // returning a value.
    if is_primitive(&actual, env.registry, "Never") {
        return;
    }
    match check_compatible(trailing, &actual, declared, env.registry) {
        Compatible::Strict => {}
        Compatible::Coerced(width) => {
            *coercion_target_mut(trailing) = Some(LiteralCoercion::NumericLiteralWidth(width));
        }
        Compatible::UnionWiden { target } => {
            *coercion_annotation_mut(trailing) = Some(Coercion::UnionWiden(target));
        }
        Compatible::OutOfRange {
            rendered_value,
            width,
        } => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "return value `{rendered_value}` does not fit `{}`'s declared \
                     return type `{}` (range {})",
                    function.name,
                    width.label(),
                    width.range_label(),
                ),
                trailing.span,
            ));
        }
        Compatible::Incompatible => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "return type mismatch on `{}`: expected `{}`, found `{}`",
                    function.name,
                    display_resolution(declared, env.registry),
                    display_resolution(&actual, env.registry),
                ),
                trailing.span,
            ));
        }
    }
}

fn statement_span(statement: &Statement) -> koja_ast::span::Span {
    match statement {
        Statement::Assignment { span, .. }
        | Statement::Break { span }
        | Statement::CompoundAssign { span, .. }
        | Statement::Return { span, .. } => *span,
        Statement::Expr(expr) => expr.span,
    }
}
