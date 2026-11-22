//! Return-position checking of the trailing expression against the
//! declared return type, and of every explicit `return` statement.
//!
//! Once resolve has walked the body of a function or a block closure,
//! every `Statement::Expr` carries a resolved type. The compiler's
//! contract is that the body's final expression is the return value.
//! [`check_return_type`] enforces the typecheck side. When the declared
//! return type is non-Unit, the trailing statement must be a `return`
//! or a [`Statement::Expr`] whose resolution is compatible with the
//! declared return type. [`check_explicit_return`] applies the same
//! compatibility rules at each `return` site.

use koja_ast::ast::{Diagnostic, Edit, Expr, Statement};
use koja_ast::identifier::ResolvedType;
use koja_ast::span::{Position, Span};

use crate::registry::GlobalRegistry;

use super::coercion::{Mismatch, check_compatible_stamping};
use super::ctx::Resolver;
use super::error_channel::{ErrorChannel, ok_unit_construction, ok_wrap_expr};
use super::types::{display_resolution, is_primitive, types_equivalent};

/// Diagnose any mismatch between the declared return type of a
/// function or a block closure and the type produced by the trailing
/// expression of its `body`. In a `!`-spelled function the trailing
/// expression checks against the unwrapped success type instead, and
/// wraps in `Result.Ok` once it passes.
///
/// The declared type and the error channel come from `resolver`, the
/// same source [`check_explicit_return`] reads, so the two checks
/// cannot disagree. `owner` is the function name for the messages,
/// and `None` for a closure. `span` places the message for an empty
/// body.
///
/// Skips the check when:
/// - No declared type is in scope.
/// - The declared (or unwrapped success) type is `Unit`. The body's
///   last value is discarded, so arbitrary trailing types are fine.
///   A `-> () ! E` function additionally gets `Result.Ok(())`
///   appended when the body can fall off the end.
/// - The declared return is `<unresolved>`. The annotation already
///   triggered its own diagnostic upstream, and piling on with a return
///   mismatch only adds noise.
/// - The trailing statement is a `return`, which
///   [`check_explicit_return`] checks at its own site.
pub(super) fn check_return_type(
    body: &mut Vec<Statement>,
    owner: Option<&str>,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(expected) = resolver.current_return_type.as_ref() else {
        return;
    };
    let registry = resolver.registry;
    let channel = resolver.error_channel.as_ref().filter(|c| c.ok_wraps);
    // In a `!`-spelled function the declared type is the full
    // `Result`, and `expected` is its success type.
    let declared = channel.map_or(expected, |c| &c.result);
    if !declared.is_resolved() {
        return;
    }
    if is_primitive(expected, registry, "Unit") {
        if let Some(channel) = channel
            && body_can_fall_off(body, registry)
        {
            let span = body.last().map_or(span, statement_span);
            body.push(Statement::Expr(ok_unit_construction(
                &channel.result,
                span,
                registry,
            )));
        }
        return;
    }
    let Some(last) = body.last_mut() else {
        diagnostics.push(Diagnostic::error(
            format!(
                "return type mismatch{}. Expected `{}`, found empty body",
                owner_site(owner),
                display_resolution(expected, registry),
            ),
            span,
        ));
        return;
    };
    // A trailing `return` (including a desugared `fail`) already
    // checked at its own site, and the body cannot fall off the end
    // past it.
    if matches!(last, Statement::Return { .. }) {
        return;
    }
    let last_span = statement_span(last);
    let Statement::Expr(trailing) = last else {
        diagnostics.push(Diagnostic::error(
            format!(
                "return type mismatch{}. Expected `{}`, found a non-expression \
                 trailing statement",
                owner_site(owner),
                display_resolution(expected, registry),
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
    // `Never` is the lattice bottom. A body that diverges (e.g. its
    // trailing expression is `if cond then return 1 else return 2 end`,
    // or a bare `panic()` call) satisfies any non-`Never` declared
    // return type without ever actually returning a value.
    if is_primitive(&actual, registry, "Never") {
        return;
    }
    if let Some(mismatch) = check_compatible_stamping(trailing, &actual, expected, registry) {
        let message = mismatch_message(mismatch, expected, &actual, owner, registry);
        let hint = auto_wrap_hint(channel, &actual, registry);
        diagnostics.push(mismatch_diagnostic(message, hint, trailing.span));
        return;
    }
    if let Some(channel) = channel {
        ok_wrap_expr(trailing, &channel.result);
    }
}

/// True when execution can reach the end of `body` and fall off:
/// the body is empty or its last statement neither returns nor
/// diverges.
fn body_can_fall_off(body: &[Statement], registry: &GlobalRegistry) -> bool {
    match body.last() {
        None => true,
        Some(Statement::Return { .. }) => false,
        Some(Statement::Expr(expr)) => !is_primitive(&expr.resolution, registry, "Never"),
        Some(_) => true,
    }
}

/// A `!`-spelled function whose return site produces the full
/// `Result` type is almost always a hand-written `Result.Ok(...)`.
/// Surface the auto-wrap rule instead of a bare type mismatch.
fn auto_wrap_hint(
    channel: Option<&ErrorChannel>,
    actual: &ResolvedType,
    registry: &GlobalRegistry,
) -> Option<String> {
    let channel = channel.filter(|c| c.ok_wraps)?;
    if !types_equivalent(actual, &channel.result, registry) {
        return None;
    }
    Some(
        "this function wraps success values in `Result.Ok` automatically, so return \
         the plain value (or use `fail` for the error path)"
            .to_string(),
    )
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
                    "return is missing a value. This function returns `{}`",
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
            span,
            value.span,
        ));
        return;
    }
    if let Some(mismatch) = check_compatible_stamping(value, &actual, declared, registry) {
        let message = mismatch_message(mismatch, declared, &actual, None, registry);
        let hint = auto_wrap_hint(resolver.error_channel.as_ref(), &actual, registry);
        diagnostics.push(mismatch_diagnostic(message, hint, value.span));
    }
}

/// The valued-`return` rejection for Unit-returning bodies. Scripts
/// get their own wording since they have no return channel at all
/// (exit codes go through `Kernel.exit`). In a function the fix
/// deletes everything after the `return` keyword, which `statement`
/// starts with, through the value.
fn unit_return_value_diagnostic(in_script_body: bool, statement: Span, value: Span) -> Diagnostic {
    if in_script_body {
        return Diagnostic::error_with_hint(
            "scripts do not return a value",
            "use `Kernel.exit(code)` to set an exit code, or print the value",
            value,
        );
    }
    let keyword_end = Position {
        offset: statement.start.offset + RETURN_KEYWORD_LEN,
        line: statement.start.line,
        column: statement.start.column + RETURN_KEYWORD_LEN,
    };
    let after_keyword = Span::new(keyword_end, value.end, statement.file);
    Diagnostic::error_with_hint(
        "cannot return a value from a function that returns `Unit`",
        "use a bare `return`",
        value,
    )
    .with_fix("Drop the return value", vec![Edit::delete(after_keyword)])
}

/// Byte and column length of the `return` keyword.
const RETURN_KEYWORD_LEN: u32 = "return".len() as u32;

/// Render a return-position [`Mismatch`] into its message. `owner`
/// is the function name for the trailing-expression check, `None`
/// for an explicit `return` (which may sit inside an anonymous
/// closure).
fn mismatch_message(
    mismatch: Mismatch,
    declared: &ResolvedType,
    actual: &ResolvedType,
    owner: Option<&str>,
    registry: &GlobalRegistry,
) -> String {
    match mismatch {
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
        Mismatch::Incompatible => format!(
            "return type mismatch{}. Expected `{}`, found `{}`",
            owner_site(owner),
            display_resolution(declared, registry),
            display_resolution(actual, registry),
        ),
    }
}

/// The part of a return mismatch message that names the function.
/// Empty for a closure and for an explicit `return`, which have no
/// name to show.
fn owner_site(owner: Option<&str>) -> String {
    owner.map_or_else(String::new, |name| format!(" on `{name}`"))
}

/// Assemble the mismatch diagnostic, attaching the
/// [`auto_wrap_hint`] when present.
fn mismatch_diagnostic(message: String, hint: Option<String>, span: Span) -> Diagnostic {
    match hint {
        Some(hint) => Diagnostic::error_with_hint(message, hint, span),
        None => Diagnostic::error(message, span),
    }
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
