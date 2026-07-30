//! Return-position checking: the trailing expression against the
//! declared return type, and every explicit `return` statement.
//!
//! Once `resolve_function` has walked the body, every `Statement::Expr`
//! carries a resolved type. The compiler's contract is that the body's
//! final expression is the function's return value. [`check_return_type`]
//! enforces the typecheck side: when the declared return type is
//! non-Unit, the trailing statement must be a [`Statement::Expr`] whose
//! resolution equals the declared return type. [`check_explicit_return`]
//! applies the same compatibility rules at each `return` site.

use koja_ast::ast::{Diagnostic, Expr, Function, Statement};
use koja_ast::identifier::ResolvedType;
use koja_ast::span::Span;

use crate::registry::{FunctionSignature, GlobalRegistry};

use super::coercion::{Mismatch, check_compatible_stamping};
use super::ctx::{Resolver, ResolverEnv};
use super::types::{display_resolution, is_primitive};

/// Diagnose any mismatch between the function's declared return type
/// and the type produced by its trailing expression.
///
/// Skips the check when:
/// - The declared return is `Unit`. The body's last value is discarded
///   and the function returns `()`, so arbitrary trailing types are fine.
/// - The declared return is `<unresolved>`. The annotation already
///   triggered its own diagnostic upstream, and piling on with a return
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
        // Trailing expression already triggered its own diagnostic.
        // Skip to avoid pile-on noise.
        return;
    }
    // `Never` is the lattice bottom: a body that diverges (e.g. its
    // trailing expression is `if cond then return 1 else return 2 end`,
    // or a bare `panic()` call) satisfies any non-`Never` declared
    // return type without ever actually returning a value.
    if is_primitive(&actual, env.registry, "Never") {
        return;
    }
    if let Some(mismatch) = check_compatible_stamping(trailing, &actual, declared, env.registry) {
        push_mismatch_diagnostic(
            mismatch,
            declared,
            &actual,
            Some(&function.name),
            trailing.span,
            env.registry,
            diagnostics,
        );
    }
}

/// Typecheck an explicit `return` statement against the innermost
/// declared return type, applying [`check_return_type`]'s rules
/// (coercion stamping, `Never` carve-out, unresolved skips) at the
/// `return` site. Additionally rejects a bare `return` in a valued
/// function and a valued `return` in a Unit-returning body, with
/// script-tailored wording for the latter. Skipped when no declared
/// type is in scope (unannotated closure under inference).
pub(super) fn check_explicit_return(
    value: Option<&mut Expr>,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(declared) = resolver.current_return_type.as_ref() else {
        return;
    };
    let registry = resolver.registry;
    if !declared.is_resolved() {
        return;
    }
    let declares_unit = is_primitive(declared, registry, "Unit");
    let Some(value) = value else {
        if !declares_unit {
            diagnostics.push(Diagnostic::error(
                format!(
                    "return is missing a value: this function returns `{}`",
                    display_resolution(declared, registry),
                ),
                span,
            ));
        }
        return;
    };
    let actual = value.resolution.clone();
    if !actual.is_resolved() || is_primitive(&actual, registry, "Never") {
        return;
    }
    if declares_unit {
        diagnostics.push(unit_return_value_diagnostic(
            resolver.in_script_body,
            value.span,
        ));
        return;
    }
    if let Some(mismatch) = check_compatible_stamping(value, &actual, declared, registry) {
        push_mismatch_diagnostic(
            mismatch,
            declared,
            &actual,
            None,
            value.span,
            registry,
            diagnostics,
        );
    }
}

/// The valued-`return` rejection for Unit-returning bodies. Scripts
/// get their own wording since they have no return channel at all
/// (exit codes go through `Kernel.exit`).
fn unit_return_value_diagnostic(in_script_body: bool, span: Span) -> Diagnostic {
    if in_script_body {
        Diagnostic::error_with_hint(
            "scripts do not return a value",
            "use `Kernel.exit(code)` to set an exit code, or print the value",
            span,
        )
    } else {
        Diagnostic::error_with_hint(
            "cannot return a value from a function that returns `Unit`",
            "use a bare `return`",
            span,
        )
    }
}

/// Render a return-position [`Mismatch`] into its diagnostic. `owner`
/// is the function name for the trailing-expression check, `None` for
/// an explicit `return` (which may sit inside an anonymous closure).
fn push_mismatch_diagnostic(
    mismatch: Mismatch,
    declared: &ResolvedType,
    actual: &ResolvedType,
    owner: Option<&str>,
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let message = match mismatch {
        Mismatch::OutOfRange {
            rendered_value,
            width,
        } => {
            let target = match owner {
                Some(name) => format!("`{name}`'s declared return type"),
                None => "the declared return type".to_string(),
            };
            format!(
                "return value `{rendered_value}` does not fit {target} `{}` (range {})",
                width.label(),
                width.range_label(),
            )
        }
        Mismatch::Incompatible => {
            let site = match owner {
                Some(name) => format!(" on `{name}`"),
                None => String::new(),
            };
            format!(
                "return type mismatch{site}: expected `{}`, found `{}`",
                display_resolution(declared, registry),
                display_resolution(actual, registry),
            )
        }
    };
    diagnostics.push(Diagnostic::error(message, span));
}

fn statement_span(statement: &Statement) -> Span {
    match statement {
        Statement::Assignment { span, .. }
        | Statement::Break { span }
        | Statement::CompoundAssign { span, .. }
        | Statement::Destructure { span, .. }
        | Statement::Return { span, .. } => *span,
        Statement::Expr(expr) => expr.span,
    }
}
