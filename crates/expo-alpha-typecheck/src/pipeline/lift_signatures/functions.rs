//! Function + param signature lifting. Shared by all three sources of
//! functions (top-level, inline struct methods, impl-block methods)
//! via the [`super::SelfContext`] knob.

use expo_ast::ast::{Diagnostic, Function, Param, PassMode, TypeExpr, is_extern_c, is_intrinsic};
use expo_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

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

    if is_extern_c(&function.annotations) {
        validate_extern_c_signature(function, &identifier, &signature, registry, diagnostics);
    }

    registry.set_signature(id, signature);
}

/// Mirror of v1's `validate_ffi_signature`. Run after a function's
/// resolved signature is in hand (so type validation works against
/// `ResolvedType`s, not raw `TypeExpr`s) but before stamping it onto
/// the registry — emitting diagnostics here keeps every path through
/// alpha typecheck honest. Stamping the signature anyway preserves
/// downstream invariants (call sites can still see a `Function(Some(_))`
/// entry, so resolve doesn't double-error on every call).
///
/// Rules (parity with v1's `expo_typecheck`):
///
/// - `@extern "C"` and `@intrinsic` are mutually exclusive — both
///   describe bodyless functions but with different semantics
///   (FFI-linked vs compiler-synthesized).
/// - `@extern "C"` functions cannot have a body (the FFI symbol is
///   the implementation).
/// - `@extern "C"` functions cannot take a `self` receiver — they
///   are top-level FFI declarations, not methods.
/// - Every parameter and the return type must name an FFI-admissible
///   primitive: `Bool`, `Unit`, `Int8..UInt64`, `Float32`, `Float64`,
///   or `CPtr<T>` (any `T`). `Int`, `Float`, `String`, and any
///   user-declared struct/enum are rejected.
fn validate_extern_c_signature(
    function: &Function,
    identifier: &Identifier,
    signature: &FunctionSignature,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if is_intrinsic(&function.annotations) {
        diagnostics.push(Diagnostic::error(
            format!("`@extern \"C\"` and `@intrinsic` are mutually exclusive (on `{identifier}`)"),
            function.span,
        ));
    }
    if function.body.is_some() {
        diagnostics.push(Diagnostic::error(
            format!(
                "`@extern \"C\"` functions cannot have a body — the C symbol is the \
                 implementation (on `{identifier}`)"
            ),
            function.span,
        ));
    }
    for param in &function.params {
        if let Param::Self_ { span, .. } = param {
            diagnostics.push(Diagnostic::error(
                format!(
                    "`@extern \"C\"` functions cannot take a `self` receiver \
                     (on `{identifier}`)"
                ),
                *span,
            ));
        }
    }
    for (index, param) in function.params.iter().enumerate() {
        let Param::Regular {
            name,
            type_expr,
            span,
            ..
        } = param
        else {
            continue;
        };
        let Some(resolved) = signature.params.get(index) else {
            continue;
        };
        if !is_ffi_admissible_type(&resolved.ty, registry) {
            diagnostics.push(Diagnostic::error(
                format!(
                    "`@extern \"C\"` parameter `{name}` has type `{}`, which is not \
                     an FFI-admissible C type — admit only `Bool`, `Unit`, \
                     `Int8`..`UInt64`, `Float32`, `Float64`, or `CPtr<T>` \
                     (on `{identifier}`)",
                    type_expr_label(type_expr),
                ),
                *span,
            ));
        }
    }
    if !is_ffi_admissible_type(&signature.return_type, registry) {
        let span = function
            .return_type
            .as_ref()
            .map(type_expr_span)
            .unwrap_or(function.span);
        diagnostics.push(Diagnostic::error(
            format!(
                "`@extern \"C\"` return type is not an FFI-admissible C type — \
                 admit only `Bool`, `Unit`, `Int8`..`UInt64`, `Float32`, \
                 `Float64`, or `CPtr<T>` (on `{identifier}`)"
            ),
            span,
        ));
    }
}

/// True when `ty` is one of the explicit-width numeric primitives,
/// `Bool`, `Unit`, or `CPtr<T>` (any pointee). Mirrors v1's FFI gate.
fn is_ffi_admissible_type(ty: &ResolvedType, registry: &GlobalRegistry) -> bool {
    let Resolution::Global(id) = ty.resolution else {
        return false;
    };
    let Some(entry) = registry.get(id) else {
        return false;
    };
    if !entry.identifier.is_in_package("Global") {
        return false;
    }
    match entry.identifier.last() {
        "Bool" | "Unit" | "Int8" | "Int16" | "Int32" | "Int64" | "UInt8" | "UInt16" | "UInt32"
        | "UInt64" | "Float32" | "Float64" => ty.type_args.is_empty(),
        "CPtr" => ty.type_args.len() == 1,
        _ => false,
    }
}

/// Best-effort surface label for a [`TypeExpr`] in diagnostics. Picks
/// the head identifier — close enough for FFI rejection messaging,
/// where the user just needs a clue which type they wrote that we
/// rejected (full pretty-printing lives in `expo-fmt`).
fn type_expr_label(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. } => path.join("."),
        TypeExpr::Self_ { .. } => "Self".to_string(),
        TypeExpr::Unit { .. } => "Unit".to_string(),
        TypeExpr::Function { .. } => "<function type>".to_string(),
        TypeExpr::Union { .. } => "<union>".to_string(),
    }
}

/// Span associated with a [`TypeExpr`] for diagnostics on the
/// return-type slot.
fn type_expr_span(ty: &TypeExpr) -> Span {
    match ty {
        TypeExpr::Named { span, .. }
        | TypeExpr::Generic { span, .. }
        | TypeExpr::Unit { span }
        | TypeExpr::Self_ { span }
        | TypeExpr::Function { span, .. }
        | TypeExpr::Union { span, .. } => *span,
    }
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
