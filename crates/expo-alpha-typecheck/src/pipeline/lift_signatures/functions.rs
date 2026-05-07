//! Function + param signature lifting. Shared by all three sources of
//! functions (top-level, inline struct methods, impl-block methods)
//! via the [`super::SelfContext`] knob.

use expo_ast::ast::{Diagnostic, Function, Param, PassMode};
use expo_ast::identifier::{GlobalRegistryId, Identifier, ResolvedType};

use crate::registry::{Dispatch, FunctionSignature, GlobalKind, GlobalRegistry, ResolvedParam};

use super::SelfContext;
use super::types::{TypeParamScope, concrete_self_type, resolve_type_expr};

/// Resolve a function's param + return types and stamp the lifted
/// [`FunctionSignature`] onto its registry entry. The caller picks
/// the [`Identifier`] and supplies a `self_context` so [`lift_param`]
/// knows whether `Param::Self_` is legal in this position and which
/// struct identity types it.
///
/// The function's [`TypeParamScope`] chains its own params (innermost)
/// over its enclosing receiver's params (outermost) so generic methods
/// like `fn swap(self) -> Pair<U, T>` on `struct Pair<T, U>` see both
/// scopes resolve to their true owners (`T` → struct id, the function's
/// own `<X>` → function id).
pub(super) fn lift_function_with_identifier(
    function: &Function,
    identifier: Identifier,
    self_context: SelfContext<'_>,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
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

    let owners = type_param_owners(id, function, self_context, &identifier, registry);
    let scope = TypeParamScope::new(&owners);

    let mut params = Vec::with_capacity(function.params.len());
    for param in &function.params {
        params.push(lift_param(
            param,
            &identifier,
            self_context,
            scope,
            package,
            registry,
            diagnostics,
        ));
    }

    let return_type = match function.return_type.as_ref() {
        Some(type_expr) => resolve_type_expr(type_expr, scope, package, registry, diagnostics),
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

    registry.set_signature(id, signature);
}

/// Build the chained [`TypeParamScope`] owner stack for a function
/// being lifted. Innermost first: the function's own id (only when
/// it declares its own params) over the enclosing receiver's id
/// (always pushed for method contexts so `Self` resolves through
/// the scope walker — the type-param name lookup naturally returns
/// `None` for non-generic owners). Top-level non-generic fns
/// produce an empty stack.
///
/// Trait-impl methods (`impl P for List<T> { fn ... }`) and
/// inherent-impl methods both anchor at the receiver's id. The
/// impl block's free type-params alias the receiver's slots (e.g.
/// `T` in `impl Show for List<T>` resolves to
/// `TypeParam(List, 0)`), so a single receiver-keyed scope covers
/// every shape that has a `self` receiver.
fn type_param_owners(
    fn_id: GlobalRegistryId,
    function: &Function,
    self_context: SelfContext<'_>,
    identifier: &Identifier,
    registry: &GlobalRegistry,
) -> Vec<GlobalRegistryId> {
    let mut owners = Vec::new();
    if !function.type_params.is_empty() {
        owners.push(fn_id);
    }
    if let SelfContext::Receiver {
        receiver: receiver_identifier,
        ..
    } = self_context
    {
        let Some((receiver_id, _)) = registry.lookup(receiver_identifier) else {
            panic!(
                "lift_signatures: enclosing receiver `{receiver_identifier}` missing from \
                 registry while building type-param scope on `{identifier}` — collect \
                 invariant violation",
            );
        };
        owners.push(receiver_id);
    }
    owners
}

fn lift_param(
    param: &Param,
    identifier: &Identifier,
    self_context: SelfContext<'_>,
    scope: TypeParamScope<'_>,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedParam {
    match param {
        Param::Self_ { span, .. } => {
            let SelfContext::Receiver {
                receiver: receiver_identifier,
                self_override,
            } = self_context
            else {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "`self` receiver is only valid inside `struct`, `enum`, or `impl` \
                         blocks (on `{identifier}`)"
                    ),
                    *span,
                ));
                return ResolvedParam {
                    name: "self".to_string(),
                    ty: ResolvedType::unresolved(),
                };
            };
            let ty = match self_override {
                Some(target) => target.clone(),
                None => {
                    let Some((receiver_id, _)) = registry.lookup(receiver_identifier) else {
                        panic!(
                            "lift_signatures: enclosing receiver `{receiver_identifier}` \
                             missing from registry while lifting `self` on `{identifier}` — \
                             collect invariant violation",
                        );
                    };
                    concrete_self_type(receiver_id, registry)
                }
            };
            ResolvedParam {
                name: "self".to_string(),
                ty,
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
            let ty = resolve_type_expr(type_expr, scope, package, registry, diagnostics);
            ResolvedParam {
                name: name.clone(),
                ty,
            }
        }
    }
}
