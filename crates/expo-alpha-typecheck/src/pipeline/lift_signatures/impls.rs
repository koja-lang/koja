//! Inherent + trait impl lifting. Inherent impls forward each member
//! to [`functions::lift_function_with_identifier`]. Trait impls
//! additionally check protocol conformance and synthesize any
//! default-bodied protocol methods that the impl omitted.

use std::collections::HashMap;

use expo_ast::ast::{Diagnostic, Function, ImplBlock, ImplMember, ProtocolMethod, Visibility};
use expo_ast::identifier::Identifier;

use crate::registry::{
    Dispatch, GlobalKind, GlobalRegistry, InsertOutcome, ProtocolDefinition, ResolvedProtocolMethod,
};

use super::ProtocolBodies;
use super::SelfContext;
use super::functions::lift_function_with_identifier;
use super::types::{dispatch_label, impl_target_name, render_resolved, type_expr_span};

/// Recurring args threaded through trait-impl handling. Pure data
/// bundle; helpers take it by value (everything inside is a borrow).
#[derive(Clone, Copy)]
struct ProtocolImplCtx<'a> {
    package: &'a str,
    protocol_identifier: &'a Identifier,
    target_identifier: &'a Identifier,
    target_name: &'a str,
}

pub(super) fn lift_impl(
    impl_block: &mut ImplBlock,
    package: &str,
    bodies: &ProtocolBodies,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(target_name) = impl_target_name(&impl_block.target).map(str::to_string) else {
        return;
    };
    let target_identifier = Identifier::new(package, vec![target_name.clone()]);
    if !matches!(
        registry.lookup(&target_identifier).map(|(_, e)| &e.kind),
        Some(GlobalKind::Enum(_) | GlobalKind::Struct(_))
    ) {
        // Collect already diagnosed; nothing was registered.
        return;
    }
    for member in &impl_block.members {
        let ImplMember::Function(function) = member else {
            continue;
        };
        let method_identifier =
            Identifier::new(package, vec![target_name.clone(), function.name.clone()]);
        lift_function_with_identifier(
            function,
            method_identifier,
            SelfContext::Receiver(&target_identifier),
            package,
            registry,
            diagnostics,
        );
    }
    if impl_block.trait_expr.is_some() {
        verify_and_synthesize_trait_impl(
            impl_block,
            &target_name,
            &target_identifier,
            package,
            bodies,
            registry,
            diagnostics,
        );
    }
}

fn verify_and_synthesize_trait_impl(
    impl_block: &mut ImplBlock,
    target_name: &str,
    target_identifier: &Identifier,
    package: &str,
    bodies: &ProtocolBodies,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let trait_expr = impl_block
        .trait_expr
        .as_ref()
        .expect("verify_and_synthesize_trait_impl called on inherent impl");
    let Some(protocol_name) = impl_target_name(trait_expr) else {
        return;
    };
    let protocol_identifier = Identifier::new(package, vec![protocol_name.to_string()]);
    let Some((protocol_id, protocol_entry)) = registry.lookup(&protocol_identifier) else {
        diagnostics.push(Diagnostic::error(
            format!("alpha typecheck cannot find protocol `{protocol_name}`"),
            type_expr_span(trait_expr),
        ));
        return;
    };
    let GlobalKind::Protocol(definition) = &protocol_entry.kind else {
        diagnostics.push(Diagnostic::error(
            format!(
                "`impl Trait for Type` requires a protocol on the left (`{protocol_name}` is a {})",
                protocol_entry.kind.label(),
            ),
            type_expr_span(trait_expr),
        ));
        return;
    };
    let Some(definition) = definition.clone() else {
        diagnostics.push(Diagnostic::error(
            format!(
                "internal: protocol `{protocol_name}` has no lifted definition while checking \
                 `impl ... for {target_name}`",
            ),
            impl_block.span,
        ));
        return;
    };
    let ctx = ProtocolImplCtx {
        package,
        protocol_identifier: &protocol_identifier,
        target_identifier,
        target_name,
    };
    verify_protocol_conformance(impl_block, &definition, ctx, registry, diagnostics);
    let declared: HashMap<String, ()> = impl_block
        .members
        .iter()
        .filter_map(|m| match m {
            ImplMember::Function(function) => Some((function.name.clone(), ())),
            ImplMember::TypeAlias(_) => None,
        })
        .collect();
    let to_synthesize: Vec<&ResolvedProtocolMethod> = definition
        .methods
        .iter()
        .filter(|method| method.has_default && !declared.contains_key(&method.name))
        .collect();
    for method in to_synthesize {
        let Some(default_method) = bodies
            .get(&protocol_id)
            .and_then(|m| m.get(&method.name))
            .cloned()
        else {
            diagnostics.push(Diagnostic::error(
                format!(
                    "internal: default body for `{protocol_identifier}.{}` missing from sidecar",
                    method.name,
                ),
                impl_block.span,
            ));
            continue;
        };
        synthesize_default_method(impl_block, default_method, ctx, registry, diagnostics);
    }
}

/// Clone a default `ProtocolMethod` into the impl as a synthetic
/// `Function`, register `<package>.<target_name>.<method_name>`, and
/// lift its signature against the impl target.
fn synthesize_default_method(
    impl_block: &mut ImplBlock,
    method: ProtocolMethod,
    ctx: ProtocolImplCtx<'_>,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let function = Function {
        annotations: Vec::new(),
        visibility: Visibility::Public,
        name: method.name,
        type_params: method.type_params,
        params: method.params,
        return_type: method.return_type,
        body: method.body,
        span: method.span,
    };
    let method_identifier = Identifier::new(
        ctx.package,
        vec![ctx.target_name.to_string(), function.name.clone()],
    );
    let type_params: Vec<String> = function
        .type_params
        .iter()
        .map(|p| p.name.clone())
        .collect();
    if !matches!(
        registry.insert_function(method_identifier.clone(), function.span, type_params),
        InsertOutcome::Fresh(_)
    ) {
        return;
    }
    lift_function_with_identifier(
        &function,
        method_identifier,
        SelfContext::Receiver(ctx.target_identifier),
        ctx.package,
        registry,
        diagnostics,
    );
    impl_block.members.push(ImplMember::Function(function));
}

fn verify_protocol_conformance(
    impl_block: &ImplBlock,
    definition: &ProtocolDefinition,
    ctx: ProtocolImplCtx<'_>,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let declared: HashMap<&str, &Function> = impl_block
        .members
        .iter()
        .filter_map(|member| match member {
            ImplMember::Function(function) => Some((function.name.as_str(), function)),
            ImplMember::TypeAlias(_) => None,
        })
        .collect();
    let ProtocolImplCtx {
        protocol_identifier,
        target_name,
        ..
    } = ctx;
    for method in &definition.methods {
        match declared.get(method.name.as_str()) {
            Some(impl_function) => {
                check_impl_method_signature(method, impl_function, ctx, registry, diagnostics);
            }
            None if !method.has_default => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "missing method `{}` required by protocol `{protocol_identifier}` \
                         (on `impl {protocol_identifier} for {target_name}`)",
                        method.name,
                    ),
                    impl_block.span,
                ));
            }
            None => {}
        }
    }
    let protocol_method_names: HashMap<&str, ()> = definition
        .methods
        .iter()
        .map(|m| (m.name.as_str(), ()))
        .collect();
    for member in &impl_block.members {
        let ImplMember::Function(function) = member else {
            continue;
        };
        if !protocol_method_names.contains_key(function.name.as_str()) {
            diagnostics.push(Diagnostic::error(
                format!(
                    "method `{}` is not declared in protocol `{protocol_identifier}` \
                     (on `impl {protocol_identifier} for {target_name}`)",
                    function.name,
                ),
                function.span,
            ));
        }
    }
}

/// Compare an impl method's lifted [`crate::registry::FunctionSignature`]
/// against the protocol method. One diagnostic per disagreement axis
/// (dispatch / arity / param type / return type).
fn check_impl_method_signature(
    expected: &ResolvedProtocolMethod,
    impl_function: &Function,
    ctx: ProtocolImplCtx<'_>,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let ProtocolImplCtx {
        package,
        protocol_identifier,
        target_name,
        ..
    } = ctx;
    let method_identifier = Identifier::new(
        package,
        vec![target_name.to_string(), impl_function.name.clone()],
    );
    let Some((_, entry)) = registry.lookup(&method_identifier) else {
        return;
    };
    let GlobalKind::Function(Some(actual)) = &entry.kind else {
        return;
    };
    if expected.dispatch != actual.dispatch {
        diagnostics.push(Diagnostic::error(
            format!(
                "method `{}` has the wrong receiver shape for protocol `{protocol_identifier}` \
                 (expected `{}`, got `{}`)",
                impl_function.name,
                dispatch_label(expected.dispatch),
                dispatch_label(actual.dispatch),
            ),
            impl_function.span,
        ));
        return;
    }
    let actual_non_self = match expected.dispatch {
        Dispatch::Instance => &actual.params[1..],
        Dispatch::Static => &actual.params[..],
    };
    if actual_non_self.len() != expected.non_self_params.len() {
        diagnostics.push(Diagnostic::error(
            format!(
                "method `{}` has the wrong arity for protocol `{protocol_identifier}` \
                 (expected {} param(s), got {})",
                impl_function.name,
                expected.non_self_params.len(),
                actual_non_self.len(),
            ),
            impl_function.span,
        ));
        return;
    }
    for (idx, (want, got)) in expected
        .non_self_params
        .iter()
        .zip(actual_non_self.iter())
        .enumerate()
    {
        if want.ty != got.ty {
            diagnostics.push(Diagnostic::error(
                format!(
                    "param #{} (`{}`) on method `{}` does not match protocol \
                     `{protocol_identifier}` (expected `{}`, got `{}`)",
                    idx + 1,
                    got.name,
                    impl_function.name,
                    render_resolved(&want.ty, registry),
                    render_resolved(&got.ty, registry),
                ),
                impl_function.span,
            ));
        }
    }
    if expected.return_type != actual.return_type {
        diagnostics.push(Diagnostic::error(
            format!(
                "return type of method `{}` does not match protocol `{protocol_identifier}` \
                 (expected `{}`, got `{}`)",
                impl_function.name,
                render_resolved(&expected.return_type, registry),
                render_resolved(&actual.return_type, registry),
            ),
            impl_function.span,
        ));
    }
}
