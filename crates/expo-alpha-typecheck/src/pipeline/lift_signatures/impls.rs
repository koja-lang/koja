//! Inherent + trait impl lifting. Inherent impls forward each member
//! to [`functions::lift_function_with_identifier`]. Trait impls
//! additionally check protocol conformance, synthesize any
//! default-bodied protocol methods that the impl omitted, and
//! record the conformance fact (`target : protocol`) on the
//! target's [`crate::registry::StructDefinition`] /
//! [`crate::registry::EnumDefinition`] so the receiver entry stays
//! self-contained for IR consumption.

use std::collections::HashMap;

use expo_ast::ast::{Diagnostic, Function, ImplBlock, ImplMember, ProtocolMethod, Visibility};
use expo_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};

use crate::pipeline::unify::substitute_resolved_type;
use crate::registry::{
    Dispatch, GlobalKind, GlobalRegistry, InsertOutcome, ProtocolDefinition, ResolvedProtocolMethod,
};

use super::ProtocolBodies;
use super::SelfContext;
use super::functions::lift_function_with_identifier;
use super::types::{
    TypeParamScope, dispatch_label, impl_target_name, render_resolved, resolve_type_expr,
    type_expr_span,
};

/// Recurring args threaded through trait-impl handling. Pure data
/// bundle; helpers take it by value (everything inside is a borrow).
/// `protocol_subst` maps the protocol's type-param slots to concrete
/// types so conformance can compare apples to apples: slot 0 (`Self`)
/// is the impl's resolved target type; slots 1..N are the type-args
/// the user wrote on `trait_expr` (`Eq<String>` → `[String]`).
#[derive(Clone, Copy)]
struct ProtocolImplCtx<'a> {
    package: &'a str,
    protocol_identifier: &'a Identifier,
    target_identifier: &'a Identifier,
    target_name: &'a str,
    target: &'a ResolvedType,
    protocol_id: GlobalRegistryId,
    protocol_subst: &'a [Option<ResolvedType>],
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
    // Resolve the impl target's type expression up front so method
    // `self` types as the impl's resolved target (e.g. `Bag<Int>`
    // for `impl Bag<Int>` or `impl P for Bag<Int>`). Concrete-arg
    // specializations rely on this so the call-site receiver-type
    // check distinguishes `Bag<Int>` from `Bag<String>`. For
    // generic targets like `impl Bag<T>` the resolved target is
    // `Bag<TypeParam(Bag, 0)>`, which is identical to the
    // `concrete_self_type` shape the receiver fallback would
    // build — keeping the override always-on simplifies the
    // method-lift loop without changing behavior for the common
    // generic-aliased case.
    let resolved_target = resolve_impl_target(impl_block, &target_identifier, registry);
    let resolved = if impl_block.trait_expr.is_some() {
        resolve_protocol_impl_heads(
            impl_block,
            &target_identifier,
            &resolved_target,
            package,
            registry,
            diagnostics,
        )
    } else {
        None
    };
    let self_override = Some(&resolved_target);
    for member in &impl_block.members {
        let ImplMember::Function(function) = member else {
            continue;
        };
        let method_identifier =
            Identifier::new(package, vec![target_name.clone(), function.name.clone()]);
        lift_function_with_identifier(
            function,
            method_identifier,
            SelfContext::Receiver {
                receiver: &target_identifier,
                self_override,
            },
            package,
            registry,
            diagnostics,
        );
    }
    if impl_block.trait_expr.is_some() {
        let Some(resolved) = resolved else {
            return;
        };
        let target_id = registry
            .lookup(&target_identifier)
            .expect("target entry was checked above")
            .0;
        verify_and_synthesize_trait_impl(
            impl_block,
            &target_name,
            &target_identifier,
            package,
            &resolved,
            bodies,
            registry,
            diagnostics,
        );
        record_target_conformance(impl_block, target_id, &resolved, registry, diagnostics);
    }
}

/// Resolved `target` + `trait_expr` for an `impl P for T` block,
/// computed once in [`lift_impl`] and threaded through both
/// conformance verification and protocol-impl-entry stamping. The
/// `protocol_subst` field is the substitution vec passed to
/// [`substitute_resolved_type`] when comparing impl methods against
/// protocol methods: slot 0 (`Self`) is the resolved target, slots
/// 1..N are the type-args the user wrote on `trait_expr`.
struct ResolvedImplHeads {
    target: ResolvedType,
    protocol: ResolvedType,
    protocol_id: GlobalRegistryId,
    protocol_subst: Vec<Option<ResolvedType>>,
}

/// Resolve the impl block's target type expression under a scope
/// rooted at the target struct/enum. `T` in `impl Bag<T>` (or
/// `impl P for Bag<T>`) resolves to `TypeParam(Bag, 0)`, matching
/// how an inline method on `struct Bag<T>` would resolve `T`.
/// Concrete instantiations like `impl Bag<Int>` resolve through
/// to the global Int id.
///
/// Diagnostics from the inner [`resolve_type_expr`] are silenced
/// here — they fire again as part of normal lift via the same
/// scope, and we only want one copy on the user's screen.
fn resolve_impl_target(
    impl_block: &ImplBlock,
    target_identifier: &Identifier,
    registry: &GlobalRegistry,
) -> ResolvedType {
    let owners = impl_target_owners(target_identifier, registry);
    let scope = TypeParamScope::new(&owners);
    let mut sink = Vec::new();
    resolve_type_expr(
        &impl_block.target,
        scope,
        target_identifier.package(),
        registry,
        &mut sink,
    )
}

/// Owners list for any impl-block target scope: a single-entry
/// stack of the target struct/enum id when it carries type params,
/// empty otherwise. Shared by [`resolve_impl_target`] and
/// [`resolve_protocol_impl_heads`].
fn impl_target_owners(
    target_identifier: &Identifier,
    registry: &GlobalRegistry,
) -> Vec<GlobalRegistryId> {
    match registry.lookup(target_identifier) {
        Some((target_id, _))
            if registry
                .type_params(target_id)
                .is_some_and(|p| !p.is_empty()) =>
        {
            vec![target_id]
        }
        _ => Vec::new(),
    }
}

fn resolve_protocol_impl_heads(
    impl_block: &ImplBlock,
    target_identifier: &Identifier,
    target: &ResolvedType,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ResolvedImplHeads> {
    let trait_expr = impl_block
        .trait_expr
        .as_ref()
        .expect("resolve_protocol_impl_heads called on inherent impl");
    // Scope rooted at the target struct/enum: `T` in `Bag<T>`
    // resolves to `TypeParam(Bag, 0)`, matching how an inline
    // method on `struct Bag<T>` would resolve `T`. The impl's free
    // type-params alias the receiver's slots; we don't allocate a
    // separate impl-anchored scope.
    let owners = impl_target_owners(target_identifier, registry);
    let scope = TypeParamScope::new(&owners);
    let target = target.clone();
    let protocol = resolve_type_expr(trait_expr, scope, package, registry, diagnostics);
    let ResolvedType::Named {
        resolution: Resolution::Global(protocol_id),
        type_args: protocol_args,
    } = protocol.clone()
    else {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck cannot find protocol on `impl ... for {}`",
                target_identifier.last(),
            ),
            type_expr_span(trait_expr),
        ));
        return None;
    };
    let protocol_entry = registry.get(protocol_id)?;
    if !matches!(protocol_entry.kind, GlobalKind::Protocol(_)) {
        diagnostics.push(Diagnostic::error(
            format!(
                "`impl Trait for Type` requires a protocol on the left (`{}` is a {})",
                protocol_entry.identifier,
                protocol_entry.kind.label(),
            ),
            type_expr_span(trait_expr),
        ));
        return None;
    }
    let protocol_arity = registry
        .type_params(protocol_id)
        .map(<[String]>::len)
        .unwrap_or(0);
    // Slot 0 is the implicit `Self`; only slots 1..N are user-declared.
    let expected_user_args = protocol_arity.saturating_sub(1);
    if protocol_args.len() != expected_user_args {
        diagnostics.push(Diagnostic::error(
            format!(
                "protocol `{}` expects {expected_user_args} type argument{}, got {}",
                protocol_entry.identifier,
                if expected_user_args == 1 { "" } else { "s" },
                protocol_args.len(),
            ),
            type_expr_span(trait_expr),
        ));
        return None;
    }
    let mut subst: Vec<Option<ResolvedType>> = vec![None; protocol_arity];
    if !subst.is_empty() {
        subst[0] = Some(target.clone());
    }
    for (slot, arg) in subst.iter_mut().skip(1).zip(protocol_args.iter()) {
        *slot = Some(arg.clone());
    }
    Some(ResolvedImplHeads {
        target,
        protocol,
        protocol_id,
        protocol_subst: subst,
    })
}

/// Record `target_id : protocol_id` on the target's struct/enum
/// definition. Runs after conformance verification +
/// default-body synthesis so the conformance fact is only
/// recorded when the impl block is well-formed. Diagnoses
/// duplicate `impl P for T` blocks against the existing
/// conformance map.
fn record_target_conformance(
    impl_block: &ImplBlock,
    target_id: GlobalRegistryId,
    resolved: &ResolvedImplHeads,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let protocol_args: Vec<ResolvedType> = match &resolved.protocol {
        ResolvedType::Named { type_args, .. } => type_args.clone(),
        _ => Vec::new(),
    };
    if registry
        .record_conformance(target_id, resolved.protocol_id, protocol_args)
        .is_some()
    {
        let target_label = render_resolved(&resolved.target, registry);
        let protocol_label = render_resolved(&resolved.protocol, registry);
        diagnostics.push(Diagnostic::error(
            format!("duplicate `impl {protocol_label} for {target_label}`"),
            impl_block.span,
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn verify_and_synthesize_trait_impl(
    impl_block: &mut ImplBlock,
    target_name: &str,
    target_identifier: &Identifier,
    package: &str,
    resolved: &ResolvedImplHeads,
    bodies: &ProtocolBodies,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let protocol_id = resolved.protocol_id;
    let protocol_entry = registry.get(protocol_id).unwrap_or_else(|| {
        panic!("verify_and_synthesize_trait_impl: protocol id {protocol_id} missing")
    });
    let protocol_identifier = protocol_entry.identifier.clone();
    let GlobalKind::Protocol(Some(definition)) = &protocol_entry.kind else {
        diagnostics.push(Diagnostic::error(
            format!(
                "internal: protocol `{protocol_identifier}` has no lifted definition while \
                 checking `impl ... for {target_name}`",
            ),
            impl_block.span,
        ));
        return;
    };
    let definition = definition.clone();
    let ctx = ProtocolImplCtx {
        package,
        protocol_identifier: &protocol_identifier,
        target_identifier,
        target_name,
        target: &resolved.target,
        protocol_id,
        protocol_subst: &resolved.protocol_subst,
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
        SelfContext::Receiver {
            receiver: ctx.target_identifier,
            self_override: Some(ctx.target),
        },
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
        protocol_id,
        protocol_subst,
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
        let expected_ty = substitute_resolved_type(&want.ty, protocol_subst, protocol_id);
        if expected_ty != got.ty {
            diagnostics.push(Diagnostic::error(
                format!(
                    "param #{} (`{}`) on method `{}` does not match protocol \
                     `{protocol_identifier}` (expected `{}`, got `{}`)",
                    idx + 1,
                    got.name,
                    impl_function.name,
                    render_resolved(&expected_ty, registry),
                    render_resolved(&got.ty, registry),
                ),
                impl_function.span,
            ));
        }
    }
    let expected_return =
        substitute_resolved_type(&expected.return_type, protocol_subst, protocol_id);
    if expected_return != actual.return_type {
        diagnostics.push(Diagnostic::error(
            format!(
                "return type of method `{}` does not match protocol `{protocol_identifier}` \
                 (expected `{}`, got `{}`)",
                impl_function.name,
                render_resolved(&expected_return, registry),
                render_resolved(&actual.return_type, registry),
            ),
            impl_function.span,
        ));
    }
}
