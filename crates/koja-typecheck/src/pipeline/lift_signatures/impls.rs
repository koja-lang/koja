//! Inherent + trait impl lifting. Inherent impls forward each member
//! to [`functions::lift_function_with_identifier`]. Trait impls
//! additionally check protocol conformance, synthesize any
//! default-bodied protocol methods that the impl omitted, and
//! record the conformance fact (`target : protocol`) on the
//! target's [`crate::registry::StructDefinition`] /
//! [`crate::registry::EnumDefinition`] so the receiver entry stays
//! self-contained for IR consumption.

use std::collections::HashMap;

use koja_ast::ast::{
    Diagnostic, Expr, ExprKind, ExtendBlock, Function, ImplBlock, ImplMember, MatchArm, Param,
    Pattern, ProtocolMethod, Statement, StringPart, TypeExpr, Visibility,
};
use koja_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};

use crate::pipeline::collect::{lookup_owner_path, nominal_target_path};
use crate::pipeline::unify::{Substitution, substitute};
use crate::registry::{
    Dispatch, GlobalKind, GlobalRegistry, InsertOutcome, ProtocolDefinition,
    ResolvedProtocolMethod, VisibilityScope,
};

use super::LiftScope;
use super::ProtocolBodies;
use super::SelfContext;
use super::functions::lift_function_with_identifier;
use super::types::{
    TypeParamScope, dispatch_label, render_resolved, resolve_type_expr, type_expr_span,
};

/// Read-only data bundle threaded through trait-impl conformance.
/// `Copy` so helpers can take it by value (every field is a borrow).
///
/// `protocol_subst` maps the protocol's type-param slots to concrete
/// types so conformance can compare apples to apples: slot 0 (`Self`)
/// is the impl's resolved target type, slots 1..N are the type-args
/// the user wrote on `trait_expr` (`Eq<String>` -> `[String]`).
#[derive(Clone, Copy)]
struct ProtocolImplScope<'a> {
    package: &'a str,
    /// Registry id for the protocol, needed by default-method
    /// synthesis to recover the protocol's type-param names from
    /// [`crate::registry::GlobalRegistry::type_params`].
    protocol_id: GlobalRegistryId,
    protocol_identifier: &'a Identifier,
    protocol_subst: &'a Substitution,
    target: &'a ResolvedType,
    target_identifier: &'a Identifier,
    target_path: &'a [String],
    /// User-supplied protocol type-args from `impl P<A, B, C> for T`,
    /// in source order. Used by default-method synthesis to
    /// substitute references to the protocol's type-params (`M`,
    /// `R`, …) inside the cloned default body before lift sees it.
    trait_expr: &'a TypeExpr,
}

pub(super) fn lift_impl(
    impl_block: &mut ImplBlock,
    bodies: &ProtocolBodies,
    scope: &mut LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(target_path) = nominal_target_path(&impl_block.target).map(<[String]>::to_vec) else {
        return;
    };
    let target_identifier = Identifier::new(scope.package, target_path.clone());
    if !matches!(
        scope
            .registry
            .lookup(&target_identifier)
            .map(|(_, e)| &e.kind),
        Some(GlobalKind::Enum(_) | GlobalKind::Struct(_))
    ) {
        // Collect already diagnosed. Nothing was registered.
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
    // build. Keeping the override always-on simplifies the
    // method-lift loop without changing behavior for the common
    // generic-aliased case.
    let resolved_target = resolve_impl_target(impl_block, &target_identifier, scope);
    let resolved = resolve_protocol_impl_heads(
        impl_block,
        &target_identifier,
        &resolved_target,
        scope,
        diagnostics,
    );
    let self_override = Some(&resolved_target);
    for member in &impl_block.members {
        let ImplMember::Function(function) = member else {
            continue;
        };
        let method_identifier = Identifier::member(scope.package, &target_path, &function.name);
        lift_function_with_identifier(
            function,
            method_identifier,
            SelfContext::Receiver {
                receiver: &target_identifier,
                self_override,
            },
            scope,
            diagnostics,
        );
    }
    let Some(resolved) = resolved else {
        return;
    };
    let target_id = scope
        .registry
        .lookup(&target_identifier)
        .expect("target entry was checked above")
        .0;
    verify_and_synthesize_trait_impl(
        impl_block,
        &target_path,
        &target_identifier,
        &resolved,
        bodies,
        scope,
        diagnostics,
    );
    record_target_conformance(
        impl_block,
        target_id,
        &resolved,
        scope.registry,
        diagnostics,
    );
}

/// Lift every method in an `extend Type ... end` block. Like
/// [`lift_impl`] without the protocol-conformance work, and keyed
/// by the target's own package.
pub(super) fn lift_extend(
    extend_block: &mut ExtendBlock,
    scope: &mut LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(path) = nominal_target_path(&extend_block.target) else {
        return;
    };
    let Some((_, target_package, target_path)) =
        lookup_owner_path(path, scope.package, scope.registry)
    else {
        return;
    };
    let target_identifier = Identifier::new(target_package.as_str(), target_path.clone());
    if !matches!(
        scope
            .registry
            .lookup(&target_identifier)
            .map(|(_, e)| &e.kind),
        Some(GlobalKind::Enum(_) | GlobalKind::Struct(_))
    ) {
        return;
    }
    let resolved_target = resolve_block_target(&extend_block.target, &target_identifier, scope);
    let self_override = Some(&resolved_target);
    for member in &extend_block.members {
        let ImplMember::Function(function) = member else {
            continue;
        };
        let method_identifier =
            Identifier::member(target_package.as_str(), &target_path, &function.name);
        lift_function_with_identifier(
            function,
            method_identifier,
            SelfContext::Receiver {
                receiver: &target_identifier,
                self_override,
            },
            scope,
            diagnostics,
        );
    }
}

/// Resolved `target` + `trait_expr` for an `impl P for T` block,
/// computed once in [`lift_impl`] and threaded through both
/// conformance verification and protocol-impl-entry stamping. The
/// `protocol_subst` field is the [`Substitution`] threaded through
/// [`substitute`] when comparing impl methods against protocol
/// methods: slot 0 (`Self`) is the resolved target, slots 1..N are
/// the type-args the user wrote on `trait_expr`.
struct ResolvedImplHeads {
    protocol: ResolvedType,
    protocol_id: GlobalRegistryId,
    protocol_subst: Substitution,
    target: ResolvedType,
}

/// Resolve the impl block's target type expression under a scope
/// rooted at the target struct/enum. `T` in `impl Bag<T>` (or
/// `impl P for Bag<T>`) resolves to `TypeParam(Bag, 0)`, matching
/// how an inline method on `struct Bag<T>` would resolve `T`.
/// Concrete instantiations like `impl Bag<Int>` resolve through
/// to the global Int id.
///
/// Diagnostics from the inner [`resolve_type_expr`] are silenced
/// here: they fire again as part of normal lift via the same
/// scope, and we only want one copy on the user's screen.
fn resolve_impl_target(
    impl_block: &ImplBlock,
    target_identifier: &Identifier,
    scope: &LiftScope<'_>,
) -> ResolvedType {
    resolve_block_target(&impl_block.target, target_identifier, scope)
}

/// Shared resolver for `impl`/`extend` target type expressions:
/// the target's own type-params resolve via [`TypeParamScope`].
fn resolve_block_target(
    target: &TypeExpr,
    target_identifier: &Identifier,
    scope: &LiftScope<'_>,
) -> ResolvedType {
    let owners = impl_target_owners(target_identifier, scope.registry);
    let type_params = TypeParamScope::new(&owners);
    let mut sink = Vec::new();
    resolve_type_expr(target, type_params, scope.resolution_scope(), &mut sink)
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
    scope: &LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ResolvedImplHeads> {
    let trait_expr = &impl_block.trait_expr;
    // Scope rooted at the target struct/enum: `T` in `Bag<T>`
    // resolves to `TypeParam(Bag, 0)`, matching how an inline
    // method on `struct Bag<T>` would resolve `T`. The impl's free
    // type-params alias the receiver's slots. We don't allocate a
    // separate impl-anchored scope.
    let owners = impl_target_owners(target_identifier, scope.registry);
    let type_params = TypeParamScope::new(&owners);
    let target = target.clone();
    let protocol = resolve_type_expr(
        trait_expr,
        type_params,
        scope.resolution_scope(),
        diagnostics,
    );
    let ResolvedType::Named {
        resolution: Resolution::Global(protocol_id),
        type_args: protocol_args,
    } = protocol.clone()
    else {
        diagnostics.push(Diagnostic::error(
            format!(
                "typecheck cannot find protocol on `impl ... for {}`",
                target_identifier.last(),
            ),
            type_expr_span(trait_expr),
        ));
        return None;
    };
    let protocol_entry = scope.registry.get(protocol_id)?;
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
    let protocol_arity = scope
        .registry
        .type_params(protocol_id)
        .map(<[String]>::len)
        .unwrap_or(0);
    // Slot 0 is the implicit `Self`. Only slots 1..N are user-declared.
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
    let mut args: Vec<ResolvedType> = Vec::with_capacity(protocol_arity);
    if protocol_arity > 0 {
        args.push(target.clone());
        args.extend(protocol_args.iter().cloned());
    }
    let protocol_subst = Substitution::from_args(protocol_id, &args);
    Some(ResolvedImplHeads {
        protocol,
        protocol_id,
        protocol_subst,
        target,
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

fn verify_and_synthesize_trait_impl(
    impl_block: &mut ImplBlock,
    target_path: &[String],
    target_identifier: &Identifier,
    resolved: &ResolvedImplHeads,
    bodies: &ProtocolBodies,
    scope: &mut LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let protocol_id = resolved.protocol_id;
    let protocol_entry = scope.registry.get(protocol_id).unwrap_or_else(|| {
        panic!("verify_and_synthesize_trait_impl: protocol id {protocol_id} missing")
    });
    let protocol_identifier = protocol_entry.identifier.clone();
    let GlobalKind::Protocol(Some(definition)) = &protocol_entry.kind else {
        diagnostics.push(Diagnostic::error(
            format!(
                "internal: protocol `{protocol_identifier}` has no lifted definition while \
                 checking `impl ... for {}`",
                target_path.join("."),
            ),
            impl_block.span,
        ));
        return;
    };
    let definition = definition.clone();
    let trait_expr = impl_block.trait_expr.clone();
    let impl_scope = ProtocolImplScope {
        package: scope.package,
        protocol_id,
        protocol_identifier: &protocol_identifier,
        protocol_subst: &resolved.protocol_subst,
        target: &resolved.target,
        target_identifier,
        target_path,
        trait_expr: &trait_expr,
    };
    verify_protocol_conformance(
        impl_block,
        &definition,
        impl_scope,
        scope.registry,
        diagnostics,
    );
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
        synthesize_default_method(impl_block, default_method, impl_scope, scope, diagnostics);
    }
}

/// Clone a default `ProtocolMethod` into the impl as a synthetic
/// `Function`, register `<package>.<target_path…>.<method_name>`, and
/// lift its signature against the impl target.
fn synthesize_default_method(
    impl_block: &mut ImplBlock,
    method: ProtocolMethod,
    impl_scope: ProtocolImplScope<'_>,
    scope: &mut LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut function = Function {
        annotations: Vec::new(),
        visibility: Visibility::Public,
        name: method.name,
        type_params: method.type_params,
        params: method.params,
        return_type: method.return_type,
        body: method.body,
        span: method.span,
    };
    substitute_protocol_type_params(&mut function, impl_scope, scope);
    let method_identifier =
        Identifier::member(impl_scope.package, impl_scope.target_path, &function.name);
    let type_params: Vec<String> = function
        .type_params
        .iter()
        .map(|p| p.name.clone())
        .collect();
    // Synthesized protocol-default methods are always public: the
    // protocol itself declared them, and `ProtocolMethod` doesn't
    // carry a `Visibility` field at the AST level. They register
    // under the target type's name like any other method.
    if !matches!(
        scope.registry.insert_function(
            method_identifier.clone(),
            function.span,
            type_params,
            VisibilityScope::Public,
        ),
        InsertOutcome::Fresh(_)
    ) {
        return;
    }
    lift_function_with_identifier(
        &function,
        method_identifier,
        SelfContext::Receiver {
            receiver: impl_scope.target_identifier,
            self_override: Some(impl_scope.target),
        },
        scope,
        diagnostics,
    );
    impl_block.members.push(ImplMember::Function(function));
}

/// Walk a synthesized default-method `Function` and rewrite every
/// reference to a protocol type-param (`M`, `R`, …) into the
/// concrete `TypeExpr` the impl pinned. The substitution covers
/// param signatures, the return type, and every `TypeExpr` inside
/// the body: match arms' typed-binding patterns,
/// `pair: Pair<M, Option<ReplyTo<R>>>` receive-arm payloads, let-
/// binding annotations, closures, and so on.
///
/// Without this, a default body like `Process.run`'s
/// `pair: Pair<M, Option<ReplyTo<R>>> -> ...` would carry bare `M`
/// / `R` references into the impl's `Worker.run` synthesis, where
/// the resolver has no protocol-type-param scope to look them up
/// in. Pre-substituting at synthesis time means the rest of the
/// pipeline never sees a protocol type-param outside the original
/// `protocol P<...>` declaration.
fn substitute_protocol_type_params(
    function: &mut Function,
    impl_scope: ProtocolImplScope<'_>,
    scope: &LiftScope<'_>,
) {
    let Some(protocol_param_names) = scope.registry.type_params(impl_scope.protocol_id) else {
        return;
    };
    // Slot 0 is the implicit `Self` (handled by `self_override`).
    // User-declared params start at slot 1 and pair off with the
    // user's `trait_expr` args.
    let user_param_names: &[String] = if protocol_param_names.is_empty() {
        return;
    } else {
        &protocol_param_names[1..]
    };
    let trait_args = match impl_scope.trait_expr {
        TypeExpr::Generic { args, .. } => args.as_slice(),
        _ => return,
    };
    if user_param_names.len() != trait_args.len() {
        return;
    }
    let mapping: Vec<(&str, &TypeExpr)> = user_param_names
        .iter()
        .map(String::as_str)
        .zip(trait_args.iter())
        .collect();
    for (from, to) in &mapping {
        for param in &mut function.params {
            if let Param::Regular { type_expr, .. } = param {
                substitute_named_in_type_expr(type_expr, from, to);
            }
        }
        if let Some(return_type) = &mut function.return_type {
            substitute_named_in_type_expr(return_type, from, to);
        }
        if let Some(body) = &mut function.body {
            for stmt in body {
                substitute_named_in_statement(stmt, from, to);
            }
        }
    }
}

/// Replace bare `path: [from]` `Named` / `Generic` `TypeExpr`s
/// with the concrete `to` expression. Recurses into generic
/// argument lists, function-type params and returns, and union
/// alternatives so nested references like
/// `Pair<M, Option<ReplyTo<R>>>` rewrite all the way down.
fn substitute_named_in_type_expr(type_expr: &mut TypeExpr, from: &str, to: &TypeExpr) {
    match type_expr {
        TypeExpr::Named { path, .. } if path.len() == 1 && path[0] == from => {
            *type_expr = to.clone();
        }
        TypeExpr::Named { .. } | TypeExpr::Self_ { .. } | TypeExpr::Unit { .. } => {}
        TypeExpr::Generic { path, args, .. } => {
            // A bare `M<...>` would still need rewriting if `from`
            // equals `path[0]` and `to` is itself a Generic, but
            // protocol type-params are uniformly used as zero-arg
            // names, so the realistic case is just to recurse.
            let _ = path;
            for arg in args {
                substitute_named_in_type_expr(arg, from, to);
            }
        }
        TypeExpr::Function {
            params,
            return_type,
            ..
        } => {
            for param in params {
                substitute_named_in_type_expr(param, from, to);
            }
            substitute_named_in_type_expr(return_type, from, to);
        }
        TypeExpr::Union { types, .. } => {
            for ty in types {
                substitute_named_in_type_expr(ty, from, to);
            }
        }
    }
}

fn substitute_named_in_statement(statement: &mut Statement, from: &str, to: &TypeExpr) {
    match statement {
        Statement::Expr(expr) => substitute_named_in_expr(expr, from, to),
        Statement::Assignment {
            type_annotation,
            value,
            ..
        } => {
            if let Some(annotation) = type_annotation {
                substitute_named_in_type_expr(annotation, from, to);
            }
            substitute_named_in_expr(value, from, to);
        }
        Statement::CompoundAssign { value, .. } => substitute_named_in_expr(value, from, to),
        Statement::Return { value, .. } => {
            if let Some(value) = value {
                substitute_named_in_expr(value, from, to);
            }
        }
        Statement::Break { .. } => {}
    }
}

fn substitute_named_in_arms(arms: &mut [MatchArm], from: &str, to: &TypeExpr) {
    for arm in arms {
        substitute_named_in_pattern(&mut arm.pattern, from, to);
        if let Some(guard) = &mut arm.guard {
            substitute_named_in_expr(guard, from, to);
        }
        for stmt in &mut arm.body {
            substitute_named_in_statement(stmt, from, to);
        }
    }
}

fn substitute_named_in_pattern(pattern: &mut Pattern, from: &str, to: &TypeExpr) {
    match pattern {
        Pattern::TypedBinding { type_expr, .. } => {
            substitute_named_in_type_expr(type_expr, from, to);
        }
        Pattern::Or { patterns, .. }
        | Pattern::List {
            elements: patterns, ..
        } => {
            for pat in patterns {
                substitute_named_in_pattern(pat, from, to);
            }
        }
        Pattern::EnumTuple { elements, .. } | Pattern::Constructor { elements, .. } => {
            for pat in elements {
                substitute_named_in_pattern(pat, from, to);
            }
        }
        Pattern::EnumStruct { fields, .. } | Pattern::Struct { fields, .. } => {
            for field in fields {
                substitute_named_in_pattern(&mut field.pattern, from, to);
            }
        }
        Pattern::Wildcard { .. }
        | Pattern::Literal { .. }
        | Pattern::Binary { .. }
        | Pattern::Binding { .. }
        | Pattern::EnumUnit { .. } => {}
    }
}

fn substitute_named_in_expr(expr: &mut Expr, from: &str, to: &TypeExpr) {
    match &mut expr.kind {
        ExprKind::Match { subject, arms, .. } => {
            substitute_named_in_expr(subject, from, to);
            substitute_named_in_arms(arms, from, to);
        }
        ExprKind::Receive {
            arms,
            after_timeout,
            after_body,
        } => {
            substitute_named_in_arms(arms, from, to);
            if let Some(timeout) = after_timeout {
                substitute_named_in_expr(timeout, from, to);
            }
            for stmt in after_body {
                substitute_named_in_statement(stmt, from, to);
            }
        }
        ExprKind::Closure {
            return_type, body, ..
        } => {
            if let Some(rt) = return_type {
                substitute_named_in_type_expr(rt, from, to);
            }
            for stmt in body {
                substitute_named_in_statement(stmt, from, to);
            }
        }
        ExprKind::ShortClosure { body, .. } => substitute_named_in_expr(body, from, to),
        ExprKind::Call { callee, args, .. } => {
            substitute_named_in_expr(callee, from, to);
            for arg in args {
                substitute_named_in_expr(&mut arg.value, from, to);
            }
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            substitute_named_in_expr(receiver, from, to);
            for arg in args {
                substitute_named_in_expr(&mut arg.value, from, to);
            }
        }
        ExprKind::Binary { left, right, .. } => {
            substitute_named_in_expr(left, from, to);
            substitute_named_in_expr(right, from, to);
        }
        ExprKind::Unary { operand, .. } => substitute_named_in_expr(operand, from, to),
        ExprKind::If {
            condition,
            then_body,
            else_body,
            ..
        } => {
            substitute_named_in_expr(condition, from, to);
            for stmt in then_body {
                substitute_named_in_statement(stmt, from, to);
            }
            if let Some(else_body) = else_body {
                for stmt in else_body {
                    substitute_named_in_statement(stmt, from, to);
                }
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            substitute_named_in_expr(condition, from, to);
            for stmt in body {
                substitute_named_in_statement(stmt, from, to);
            }
        }
        ExprKind::For { iterable, body, .. } => {
            substitute_named_in_expr(iterable, from, to);
            for stmt in body {
                substitute_named_in_statement(stmt, from, to);
            }
        }
        ExprKind::Loop { body, .. } => {
            for stmt in body {
                substitute_named_in_statement(stmt, from, to);
            }
        }
        ExprKind::Cond {
            arms, else_body, ..
        } => {
            for arm in arms {
                substitute_named_in_expr(&mut arm.condition, from, to);
                for stmt in &mut arm.body {
                    substitute_named_in_statement(stmt, from, to);
                }
            }
            if let Some(else_body) = else_body {
                for stmt in else_body {
                    substitute_named_in_statement(stmt, from, to);
                }
            }
        }
        ExprKind::FieldAccess { receiver, .. } => substitute_named_in_expr(receiver, from, to),
        ExprKind::Group { expr, .. } | ExprKind::Spawn { expr, .. } => {
            substitute_named_in_expr(expr, from, to);
        }
        ExprKind::String { parts, .. } => {
            for part in parts {
                if let StringPart::Interpolation { expr, .. } = part {
                    substitute_named_in_expr(expr, from, to);
                }
            }
        }
        _ => {}
    }
}

fn verify_protocol_conformance(
    impl_block: &ImplBlock,
    definition: &ProtocolDefinition,
    impl_scope: ProtocolImplScope<'_>,
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
    let ProtocolImplScope {
        protocol_identifier,
        target_path,
        ..
    } = impl_scope;
    let target_label = target_path.join(".");
    for method in &definition.methods {
        match declared.get(method.name.as_str()) {
            Some(impl_function) => {
                check_impl_method_signature(
                    method,
                    impl_function,
                    impl_scope,
                    registry,
                    diagnostics,
                );
            }
            None if !method.has_default => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "missing method `{}` required by protocol `{protocol_identifier}` \
                         (on `impl {protocol_identifier} for {target_label}`)",
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
                     (on `impl {protocol_identifier} for {target_label}`)",
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
    impl_scope: ProtocolImplScope<'_>,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let ProtocolImplScope {
        package,
        protocol_identifier,
        protocol_subst,
        target_path,
        ..
    } = impl_scope;
    let method_identifier = Identifier::member(package, target_path, &impl_function.name);
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
        let expected_ty = substitute(&want.ty, protocol_subst);
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
    let expected_return = substitute(&expected.return_type, protocol_subst);
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
