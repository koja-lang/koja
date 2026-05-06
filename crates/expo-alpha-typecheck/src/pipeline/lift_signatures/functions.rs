//! Function + param signature lifting. Shared by all three sources of
//! functions (top-level, inline struct methods, impl-block methods)
//! via the [`super::SelfContext`] knob.

use expo_ast::ast::{Diagnostic, Function, Param, PassMode};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};

use crate::registry::{Dispatch, FunctionSignature, GlobalKind, GlobalRegistry, ResolvedParam};

use super::SelfContext;
use super::types::resolve_type_expr;

/// Resolve a function's param + return types and stamp the lifted
/// [`FunctionSignature`] onto its registry entry. The caller picks
/// the [`Identifier`] and supplies a `self_context` so [`lift_param`]
/// knows whether `Param::Self_` is legal in this position and which
/// struct identity types it.
pub(super) fn lift_function_with_identifier(
    function: &Function,
    identifier: Identifier,
    self_context: SelfContext<'_>,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !function.type_params.is_empty() {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support generic functions \
                 (`{identifier}` has type parameters)",
            ),
            function.span,
        ));
    }

    let mut params = Vec::with_capacity(function.params.len());
    for param in &function.params {
        params.push(lift_param(
            param,
            &identifier,
            self_context,
            package,
            registry,
            diagnostics,
        ));
    }

    let return_type = match function.return_type.as_ref() {
        Some(type_expr) => resolve_type_expr(type_expr, package, registry, diagnostics),
        None => registry.primitive("Unit"),
    };

    let dispatch = match function.params.first() {
        Some(Param::Self_ { .. }) => Dispatch::Instance,
        _ => Dispatch::Static,
    };

    let signature = FunctionSignature {
        dispatch,
        params,
        return_type,
    };

    let Some((id, entry)) = registry.lookup(&identifier) else {
        // Collect rejected this function (e.g. `self` receiver on a
        // top-level fn, collision); nothing to stamp a signature on.
        return;
    };
    // A duplicate function declaration in the same package is
    // already diagnosed by `collect`; the registry keeps the first
    // entry. If we still see a second function for this identifier,
    // its signature has already been stamped by the first walk —
    // skip to avoid tripping `set_signature`'s panic-on-double-set
    // invariant.
    if matches!(entry.kind, GlobalKind::Function(Some(_))) {
        return;
    }
    registry.set_signature(id, signature);
}

fn lift_param(
    param: &Param,
    identifier: &Identifier,
    self_context: SelfContext<'_>,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedParam {
    match param {
        Param::Self_ { span, .. } => {
            let SelfContext::Struct(struct_identifier) = self_context else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "`self` receiver is only valid inside `struct` or `impl` blocks \
                         (on `{identifier}`)"
                    ),
                    *span,
                ));
                return ResolvedParam {
                    name: "self".to_string(),
                    ty: ResolvedType::unresolved(),
                };
            };
            let Some((struct_id, _)) = registry.lookup(struct_identifier) else {
                panic!(
                    "lift_signatures: enclosing struct `{struct_identifier}` missing from \
                     registry while lifting `self` on `{identifier}` — collect invariant \
                     violation",
                );
            };
            ResolvedParam {
                name: "self".to_string(),
                ty: ResolvedType::leaf(Resolution::Global(struct_id)),
            }
        }
        Param::Regular {
            mode,
            name,
            type_expr,
            default,
            span,
            ..
        } => {
            if !matches!(mode, PassMode::Borrow) {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "alpha typecheck does not yet support `move` parameters \
                         (`{identifier}.{name}`)",
                    ),
                    *span,
                ));
            }
            if default.is_some() {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "alpha typecheck does not yet support default parameter values \
                         (`{identifier}.{name}`)",
                    ),
                    *span,
                ));
            }
            let ty = resolve_type_expr(type_expr, package, registry, diagnostics);
            ResolvedParam {
                name: name.clone(),
                ty,
            }
        }
    }
}
