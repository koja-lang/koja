//! Trailing-expression-vs-declared-return checking.
//!
//! Once `resolve_function` has walked the body, every `Statement::Expr`
//! carries a resolved type. The compiler's contract is that the body's
//! final expression is the function's return value. [`check_return_type`]
//! enforces the typecheck side: when the declared return type is
//! non-Unit, the trailing statement must be a [`Statement::Expr`] whose
//! resolution equals the declared return type.
//!
//! Mirrors v1's `check_implicit_return` ([`expo-typecheck/src/check.rs`])
//! so users see the same diagnostic shape ("return type mismatch:
//! expected `X`, found `Y`") on both pipelines.

use expo_ast::ast::{Diagnostic, Function, Statement};

use crate::registry::{FunctionSignature, GlobalRegistry};

use super::types::{display_resolution, is_primitive, types_equivalent};

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
    function: &Function,
    signature: &FunctionSignature,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(body) = function.body.as_ref() else {
        return;
    };
    let declared = &signature.return_type;
    if !declared.is_resolved() || is_primitive(declared, registry, "Unit") {
        return;
    }
    let Some(last) = body.last() else {
        diagnostics.push(Diagnostic::error(
            format!(
                "return type mismatch on `{}`: expected `{}`, found empty body",
                function.name,
                display_resolution(declared, registry),
            ),
            function.span,
        ));
        return;
    };
    let Statement::Expr(trailing) = last else {
        diagnostics.push(Diagnostic::error(
            format!(
                "return type mismatch on `{}`: expected `{}`, found a non-expression \
                 trailing statement",
                function.name,
                display_resolution(declared, registry),
            ),
            statement_span(last),
        ));
        return;
    };
    let actual = &trailing.resolution;
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
    if is_primitive(actual, registry, "Never") {
        return;
    }
    if !types_equivalent(actual, declared, registry) {
        diagnostics.push(Diagnostic::error(
            format!(
                "return type mismatch on `{}`: expected `{}`, found `{}`",
                function.name,
                display_resolution(declared, registry),
                display_resolution(actual, registry),
            ),
            trailing.span,
        ));
    }
}

fn statement_span(statement: &Statement) -> expo_ast::span::Span {
    match statement {
        Statement::Assignment { span, .. }
        | Statement::Break { span }
        | Statement::CompoundAssign { span, .. }
        | Statement::Return { span, .. } => *span,
        Statement::Expr(expr) => expr.span,
    }
}
