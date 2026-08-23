//! Protocol decl lifting: resolve each `ProtocolMethod`'s non-`self`
//! params + return type into a [`ResolvedProtocolMethod`] and stamp
//! the [`ProtocolDefinition`] onto the registry entry. Method
//! signatures resolve under a [`TypeParamScope`] rooted at the
//! protocol id so `Self` (slot 0) and user-declared `<C, M, R>`
//! params resolve to [`Resolution::TypeParam`] anchored on the
//! protocol entry.

use koja_ast::ast::{Diagnostic, Param, ProtocolDecl, ProtocolMethod};
use koja_ast::identifier::{GlobalRegistryId, Identifier};

use crate::registry::{
    Dispatch, GlobalKind, ProtocolDefinition, ResolvedParam, ResolvedProtocolMethod,
};

use super::LiftScope;
use super::types::{TypeParamScope, resolve_return_signature, resolve_type_expr};

pub(super) fn lift_protocol(
    decl: &ProtocolDecl,
    scope: &mut LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(scope.package, vec![decl.name.clone()]);
    let (id, already_lifted) = match scope.registry.lookup(&identifier) {
        Some((id, entry)) => (id, matches!(entry.kind, GlobalKind::Protocol(Some(_)))),
        None => panic!(
            "lift_signatures found protocol `{identifier}` missing from registry. This is a \
             collect invariant violation",
        ),
    };
    if already_lifted {
        // Duplicate decl already diagnosed by collect.
        return;
    }
    let mut methods: Vec<ResolvedProtocolMethod> = Vec::new();
    for method in &decl.methods {
        let resolved = lift_protocol_method(method, id, scope, diagnostics);
        if let Some(existing) = methods
            .iter()
            .find(|candidate| candidate.name == resolved.name && candidate.arity == resolved.arity)
        {
            diagnostics.push(Diagnostic::error(
                format!(
                    "protocol method `{}.{}` with arity {} is already defined",
                    decl.name, existing.name, existing.arity
                ),
                method.span,
            ));
            continue;
        }
        methods.push(resolved);
    }
    scope
        .registry
        .set_protocol_definition(id, ProtocolDefinition { methods });
}

fn lift_protocol_method(
    method: &ProtocolMethod,
    protocol_id: GlobalRegistryId,
    scope: &mut LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedProtocolMethod {
    let dispatch = match method.params.first() {
        Some(Param::Self_ { .. }) => Dispatch::Instance,
        _ => Dispatch::Static,
    };
    let owners = [protocol_id];
    let type_params = TypeParamScope::new(&owners);
    let non_self_params = method
        .params
        .iter()
        .filter_map(|param| match param {
            Param::Regular {
                name, type_expr, ..
            } => Some(ResolvedParam {
                name: name.clone(),
                ty: resolve_type_expr(
                    type_expr,
                    type_params,
                    scope.resolution_scope(),
                    diagnostics,
                ),
            }),
            Param::Self_ { .. } => None,
        })
        .collect();
    let return_type = resolve_return_signature(
        method.return_type.as_ref(),
        method.error_type.as_ref(),
        type_params,
        scope.resolution_scope(),
        diagnostics,
    );
    ResolvedProtocolMethod {
        arity: method.params.len(),
        dispatch,
        has_default: method.body.is_some(),
        name: method.name.clone(),
        non_self_params,
        return_type,
    }
}
