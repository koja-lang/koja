//! Protocol decl lifting: resolve each `ProtocolMethod`'s non-`self`
//! params + return type into a [`ResolvedProtocolMethod`] and stamp
//! the [`ProtocolDefinition`] onto the registry entry. Method
//! signatures resolve under a [`TypeParamScope`] rooted at the
//! protocol id so `Self` (slot 0) and user-declared `<C, M, R>`
//! params resolve to [`Resolution::TypeParam`] anchored on the
//! protocol entry.

use expo_ast::ast::{Diagnostic, Param, ProtocolDecl, ProtocolMethod};
use expo_ast::identifier::{GlobalRegistryId, Identifier};

use crate::registry::{
    Dispatch, GlobalKind, GlobalRegistry, ProtocolDefinition, ResolvedParam, ResolvedProtocolMethod,
};

use super::types::{TypeParamScope, resolve_type_expr};

pub(super) fn lift_protocol(
    decl: &ProtocolDecl,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(package, vec![decl.name.clone()]);
    let (id, already_lifted) = match registry.lookup(&identifier) {
        Some((id, entry)) => (id, matches!(entry.kind, GlobalKind::Protocol(Some(_)))),
        None => panic!(
            "lift_signatures: protocol `{identifier}` missing from registry — \
             collect invariant violation",
        ),
    };
    if already_lifted {
        // Duplicate decl already diagnosed by collect.
        return;
    }
    let methods = decl
        .methods
        .iter()
        .map(|method| lift_protocol_method(method, id, package, registry, diagnostics))
        .collect();
    registry.set_protocol_definition(id, ProtocolDefinition { methods });
}

fn lift_protocol_method(
    method: &ProtocolMethod,
    protocol_id: GlobalRegistryId,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedProtocolMethod {
    let dispatch = match method.params.first() {
        Some(Param::Self_ { .. }) => Dispatch::Instance,
        _ => Dispatch::Static,
    };
    let owners = [protocol_id];
    let scope = TypeParamScope::new(&owners);
    let non_self_params = method
        .params
        .iter()
        .filter_map(|param| match param {
            Param::Regular {
                mode,
                name,
                type_expr,
                ..
            } => Some(ResolvedParam {
                mode: *mode,
                name: name.clone(),
                ty: resolve_type_expr(type_expr, scope, package, registry, diagnostics),
            }),
            Param::Self_ { .. } => None,
        })
        .collect();
    let return_type = match method.return_type.as_ref() {
        Some(type_expr) => resolve_type_expr(type_expr, scope, package, registry, diagnostics),
        None => registry.primitive("Unit"),
    };
    ResolvedProtocolMethod {
        dispatch,
        has_default: method.body.is_some(),
        name: method.name.clone(),
        non_self_params,
        return_type,
    }
}
