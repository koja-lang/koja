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
use koja_ast::span::Span;

use crate::pipeline::collect::nominal_target_path;
use crate::pipeline::resolve::types::types_equivalent;
use crate::pipeline::unify::{Substitution, substitute};
use crate::registry::{
    Conformance, ConformanceScope, Dispatch, GlobalKind, GlobalRegistry, InsertOutcome,
    ProtocolDefinition, ResolvedProtocolMethod, VisibilityScope,
};

use super::LiftScope;
use super::ProtocolBodies;
use super::SelfContext;
use super::functions::{is_concrete_type, lift_function_with_identifier};
use super::types::{
    TypeParamScope, concrete_self_type, dispatch_label, render_resolved, resolve_type_expr,
    type_expr_span,
};

/// Where a conformance is declared, either an `impl P for T` block
/// or a type's conformance header (`struct T: P`). Selects
/// diagnostic wording, the member set checked against the protocol,
/// and where synthesized default methods land.
enum ConformanceSite<'a> {
    /// A header entry. Members come from the type body's functions.
    Header {
        /// `struct Server` / `enum Color`, for diagnostics.
        decl_label: &'a str,
        functions: &'a mut Vec<Function>,
        /// The protocol's entry in the header list.
        span: Span,
    },
    /// An `impl P for T` block. Members come from the block, and
    /// public non-protocol extras are rejected.
    Impl(&'a mut ImplBlock),
}

impl ConformanceSite<'_> {
    /// The functions that may satisfy the protocol's roster.
    fn declared_functions(&self) -> Vec<&Function> {
        match self {
            Self::Header { functions, .. } => functions.iter().collect(),
            Self::Impl(block) => block
                .members
                .iter()
                .filter_map(|member| match member {
                    ImplMember::Function(function) => Some(function),
                    ImplMember::TypeAlias(_) => None,
                })
                .collect(),
        }
    }

    /// Span for site-level diagnostics (missing methods, duplicates).
    fn span(&self) -> Span {
        match self {
            Self::Header { span, .. } => *span,
            Self::Impl(block) => block.span,
        }
    }

    /// Parenthetical naming the site in conformance diagnostics.
    fn context(&self, protocol: &Identifier, target: &str) -> String {
        match self {
            Self::Header { decl_label, .. } => format!("declared on `{decl_label}`"),
            Self::Impl(_) => format!("on `impl {protocol} for {target}`"),
        }
    }

    fn push_synthesized(&mut self, function: Function) {
        match self {
            Self::Header { functions, .. } => functions.push(function),
            Self::Impl(block) => block.members.push(ImplMember::Function(function)),
        }
    }
}

/// Read-only data bundle threaded through trait-impl conformance.
/// `Copy` so helpers can take it by value (every field is a borrow).
///
/// `protocol_subst` maps the protocol's type-param slots to concrete
/// types so conformance can compare apples to apples: slot 0 (`Self`)
/// is the impl's resolved target type, slots 1..N are the type-args
/// the user wrote on `trait_expr` (`Eq<String>` -> `[String]`).
#[derive(Clone, Copy)]
struct ProtocolImplScope<'a> {
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
    let Some(path) = nominal_target_path(&impl_block.target) else {
        return;
    };
    let Some((_, target_package, target_path)) =
        scope.registry.lookup_owner_path(path, scope.package)
    else {
        // Collect already diagnosed. Nothing was registered.
        return;
    };
    let target_identifier = Identifier::new(target_package.as_str(), target_path.clone());
    if !matches!(
        scope
            .registry
            .lookup(&target_identifier)
            .map(|(_, e)| &e.kind),
        Some(GlobalKind::Builtin(_) | GlobalKind::Enum(_) | GlobalKind::Struct(_))
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
    let impl_label = format!("impl ... for {}", target_identifier.last());
    let resolved = resolve_protocol_impl_heads(
        &impl_block.trait_expr,
        &target_identifier,
        &resolved_target,
        &impl_label,
        scope,
        diagnostics,
    );
    let self_override = Some(&resolved_target);
    for member in &impl_block.members {
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
    let Some(resolved) = resolved else {
        return;
    };
    let target_id = scope
        .registry
        .lookup(&target_identifier)
        .expect("target entry was checked above")
        .0;
    let foreign_target = target_package != scope.package;
    let trait_expr = impl_block.trait_expr.clone();
    let mut site = ConformanceSite::Impl(impl_block);
    verify_and_synthesize_conformance(
        &mut site,
        &trait_expr,
        &target_identifier,
        &resolved,
        bodies,
        scope,
        diagnostics,
    );
    record_target_conformance(
        &site,
        target_id,
        foreign_target,
        &resolved,
        scope.registry,
        diagnostics,
    );
}

/// Check every conformance-header entry on a struct/enum decl
/// (`struct T: P, Q`) against the type body's functions, reusing
/// the impl-block verification machinery. Synthesized default
/// methods land in `functions`, so the rest of the pipeline sees
/// them as ordinary type-body methods.
pub(super) fn lift_header_conformances(
    decl_kind: &str,
    path: &[String],
    conformances: &[TypeExpr],
    functions: &mut Vec<Function>,
    bodies: &ProtocolBodies,
    scope: &mut LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if conformances.is_empty() {
        return;
    }
    let target_identifier = Identifier::new(scope.package, path.to_vec());
    let Some((target_id, _)) = scope.registry.lookup(&target_identifier) else {
        return;
    };
    // The header has no written target expression. The target is the
    // decl itself with its own params projected, exactly `Self`.
    let resolved_target = concrete_self_type(target_id, scope.registry);
    let decl_label = format!("{decl_kind} {}", path.join("."));
    for trait_expr in conformances {
        let Some(resolved) = resolve_protocol_impl_heads(
            trait_expr,
            &target_identifier,
            &resolved_target,
            &decl_label,
            scope,
            diagnostics,
        ) else {
            continue;
        };
        let mut site = ConformanceSite::Header {
            decl_label: &decl_label,
            functions: &mut *functions,
            span: type_expr_span(trait_expr),
        };
        verify_and_synthesize_conformance(
            &mut site,
            trait_expr,
            &target_identifier,
            &resolved,
            bodies,
            scope,
            diagnostics,
        );
        // Header conformances always sit inside the target's own
        // package, so the target is never foreign here.
        record_target_conformance(
            &site,
            target_id,
            false,
            &resolved,
            scope.registry,
            diagnostics,
        );
    }
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
        scope.registry.lookup_owner_path(path, scope.package)
    else {
        return;
    };
    let target_identifier = Identifier::new(target_package.as_str(), target_path.clone());
    let target_kind = scope
        .registry
        .lookup(&target_identifier)
        .map(|(_, e)| &e.kind);
    let is_protocol = matches!(target_kind, Some(GlobalKind::Protocol(_)));
    if !is_protocol
        && !matches!(
            target_kind,
            Some(GlobalKind::Builtin(_) | GlobalKind::Enum(_) | GlobalKind::Struct(_))
        )
    {
        return;
    }
    if is_protocol {
        diagnose_protocol_extend_self_methods(extend_block, &target_identifier, diagnostics);
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

/// A protocol has no instance layout to receive on, so `extend` on
/// one only admits static methods.
fn diagnose_protocol_extend_self_methods(
    extend_block: &ExtendBlock,
    target_identifier: &Identifier,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for member in &extend_block.members {
        let ImplMember::Function(function) = member else {
            continue;
        };
        if function
            .params
            .first()
            .is_some_and(|p| matches!(p, Param::Self_ { .. }))
        {
            diagnostics.push(Diagnostic::error(
                format!(
                    "`extend` on protocol `{target_identifier}` only supports static methods \
                     (`{}` takes `self`)",
                    function.name,
                ),
                function.span,
            ));
        }
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

/// Resolve one conformance-declaring type expression (the protocol
/// side of `impl P for T`, or one entry in a `struct T: P` header)
/// into [`ResolvedImplHeads`]. `site_label` names the declaration
/// site in diagnostics (`impl ... for Server` / `struct Server`).
fn resolve_protocol_impl_heads(
    trait_expr: &TypeExpr,
    target_identifier: &Identifier,
    target: &ResolvedType,
    site_label: &str,
    scope: &LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ResolvedImplHeads> {
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
            format!("typecheck cannot find protocol on `{site_label}`"),
            type_expr_span(trait_expr),
        ));
        return None;
    };
    let protocol_entry = scope.registry.get(protocol_id)?;
    if !matches!(protocol_entry.kind, GlobalKind::Protocol(_)) {
        diagnostics.push(Diagnostic::error(
            format!(
                "conformance on `{site_label}` requires a protocol (`{}` is a {})",
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
/// definition, classified into a [`ConformanceScope`] by the
/// resolved target's instantiation. Runs after conformance
/// verification + default-body synthesis so the conformance fact
/// is only recorded when the declaring site is well-formed.
/// Diagnoses overlapping conformance declarations (a second
/// `impl P for T`, a header entry doubled by either form, or a
/// concrete impl repeating an instantiation) against the existing
/// conformance records.
fn record_target_conformance(
    site: &ConformanceSite<'_>,
    target_id: GlobalRegistryId,
    foreign_target: bool,
    resolved: &ResolvedImplHeads,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let protocol_args: Vec<ResolvedType> = match &resolved.protocol {
        ResolvedType::Named { type_args, .. } => type_args.clone(),
        _ => Vec::new(),
    };
    let Some(scope) = classify_conformance_scope(
        site,
        target_id,
        foreign_target,
        resolved,
        &protocol_args,
        registry,
        diagnostics,
    ) else {
        return;
    };
    let conformance = Conformance {
        protocol_args,
        scope,
    };
    if registry
        .record_conformance(target_id, resolved.protocol_id, conformance)
        .is_some()
    {
        let target_label = render_resolved(&resolved.target, registry);
        let protocol_label = render_resolved(&resolved.protocol, registry);
        let message = match site {
            ConformanceSite::Header { decl_label, .. } => {
                format!("duplicate conformance to `{protocol_label}` declared on `{decl_label}`")
            }
            ConformanceSite::Impl(_) => {
                format!("duplicate `impl {protocol_label} for {target_label}`")
            }
        };
        diagnostics.push(Diagnostic::error(message, site.span()));
    }
}

/// Classify the resolved impl target's instantiation into a
/// [`ConformanceScope`]. Targets that mix type parameters with
/// concrete args, and parameterized targets from another package,
/// wait on conditional conformance, so those diagnose and return
/// `None`.
fn classify_conformance_scope(
    site: &ConformanceSite<'_>,
    target_id: GlobalRegistryId,
    foreign_target: bool,
    resolved: &ResolvedImplHeads,
    protocol_args: &[ResolvedType],
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ConformanceScope> {
    let target_args = match &resolved.target {
        ResolvedType::Named { type_args, .. } => type_args.as_slice(),
        _ => &[],
    };
    if target_args.is_empty() {
        return Some(ConformanceScope::Concrete(Vec::new()));
    }
    let target_label = render_resolved(&resolved.target, registry);
    if targets_own_params_in_order(target_id, target_args, registry) {
        if foreign_target {
            diagnostics.push(Diagnostic::error(
                format!(
                    "typecheck does not yet support parameterized conformance for a type \
                     from another package (`{target_label}` waits on conditional conformance)"
                ),
                site.span(),
            ));
            return None;
        }
        return Some(ConformanceScope::Parameterized);
    }
    if !target_args.iter().all(is_concrete_type) {
        diagnostics.push(Diagnostic::error(
            format!(
                "typecheck does not yet support conformance targets that mix type \
                 parameters with concrete types (`{target_label}` waits on conditional \
                 conformance)"
            ),
            site.span(),
        ));
        return None;
    }
    if !protocol_args.iter().all(is_concrete_type) {
        let protocol_label = render_resolved(&resolved.protocol, registry);
        diagnostics.push(Diagnostic::error(
            format!(
                "protocol arguments on `impl {protocol_label} for {target_label}` must be \
                 concrete types when the target is a concrete instantiation"
            ),
            site.span(),
        ));
        return None;
    }
    Some(ConformanceScope::Concrete(target_args.to_vec()))
}

/// True when `args` is exactly the target's own param list in
/// declaration order (`Bag<T>` on `struct Bag<T>`), the shape a
/// parameterized impl or a header conformance resolves to.
fn targets_own_params_in_order(
    target_id: GlobalRegistryId,
    args: &[ResolvedType],
    registry: &GlobalRegistry,
) -> bool {
    let arity = registry.type_params(target_id).map_or(0, <[String]>::len);
    args.len() == arity
        && args.iter().enumerate().all(|(position, arg)| {
            matches!(
                arg,
                ResolvedType::Named {
                    resolution: Resolution::TypeParam { owner, index },
                    ..
                } if *owner == target_id && index.as_u32() as usize == position
            )
        })
}

fn verify_and_synthesize_conformance(
    site: &mut ConformanceSite<'_>,
    trait_expr: &TypeExpr,
    target_identifier: &Identifier,
    resolved: &ResolvedImplHeads,
    bodies: &ProtocolBodies,
    scope: &mut LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let target_path = target_identifier.path();
    let protocol_id = resolved.protocol_id;
    let protocol_entry = scope.registry.get(protocol_id).unwrap_or_else(|| {
        panic!("verify_and_synthesize_conformance: protocol id {protocol_id} missing")
    });
    let protocol_identifier = protocol_entry.identifier.clone();
    let GlobalKind::Protocol(Some(definition)) = &protocol_entry.kind else {
        diagnostics.push(Diagnostic::error(
            format!(
                "internal: protocol `{protocol_identifier}` has no lifted definition while \
                 checking conformance of `{}`",
                target_path.join("."),
            ),
            site.span(),
        ));
        return;
    };
    let definition = definition.clone();
    let impl_scope = ProtocolImplScope {
        protocol_id,
        protocol_identifier: &protocol_identifier,
        protocol_subst: &resolved.protocol_subst,
        target: &resolved.target,
        target_identifier,
        target_path,
        trait_expr,
    };
    verify_protocol_conformance(site, &definition, impl_scope, scope.registry, diagnostics);
    let declared: HashMap<String, ()> = site
        .declared_functions()
        .iter()
        .map(|function| (function.name.clone(), ()))
        .collect();
    let to_synthesize: Vec<&ResolvedProtocolMethod> = definition
        .methods
        .iter()
        .filter(|method| method.has_default && !declared.contains_key(&method.name))
        .collect();
    if matches!(site, ConformanceSite::Header { .. }) {
        warn_near_miss_defaults(
            site,
            &to_synthesize,
            &definition,
            &protocol_identifier,
            diagnostics,
        );
    }
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
                site.span(),
            ));
            continue;
        };
        synthesize_default_method(site, default_method, impl_scope, scope, diagnostics);
    }
}

/// Design-doc mitigation for the header form's typo hazard. When a
/// default-bodied method stays unimplemented and the type body has a
/// fn whose name is a near miss, the author probably meant to
/// override it. Impl blocks reject stray public fns outright, so
/// only the header path warns.
fn warn_near_miss_defaults(
    site: &ConformanceSite<'_>,
    omitted: &[&ResolvedProtocolMethod],
    definition: &ProtocolDefinition,
    protocol_identifier: &Identifier,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let candidates: Vec<&Function> = site
        .declared_functions()
        .into_iter()
        .filter(|function| !definition.methods.iter().any(|m| m.name == function.name))
        .collect();
    for method in omitted {
        for function in &candidates {
            if !names_are_near(&function.name, &method.name) {
                continue;
            }
            diagnostics.push(Diagnostic::warning_with_hint(
                format!(
                    "`{}` does not override `{protocol_identifier}.{}`, which keeps its \
                     default body",
                    function.name, method.name,
                ),
                format!("did you mean `{}`?", method.name),
                function.span,
            ));
        }
    }
}

/// Budget scales with the name so transposition typos in mid-length
/// names (`exicted` for `excited`, distance 2) still match without
/// letting short names pair with unrelated ones.
fn names_are_near(candidate: &str, target: &str) -> bool {
    let budget = (target.len() / 3).max(1);
    edit_distance(candidate, target) <= budget
}

/// Levenshtein distance over chars, two-row DP.
fn edit_distance(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b_chars.len()).collect();
    for (row, a_char) in a.chars().enumerate() {
        let mut current = vec![row + 1];
        for (col, b_char) in b_chars.iter().enumerate() {
            let substitution = previous[col] + usize::from(a_char != *b_char);
            current.push(
                substitution
                    .min(previous[col + 1] + 1)
                    .min(current[col] + 1),
            );
        }
        previous = current;
    }
    *previous.last().expect("distance row is never empty")
}

/// Clone a default `ProtocolMethod` into the declaring site as a
/// synthetic `Function`, register
/// `<package>.<target_path…>.<method_name>`, and lift its signature
/// against the conformance target.
fn synthesize_default_method(
    site: &mut ConformanceSite<'_>,
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
        error_type: method.error_type,
        body: method.body,
        span: method.span,
    };
    substitute_protocol_type_params(&mut function, impl_scope, scope);
    let method_identifier = Identifier::member(
        impl_scope.target_identifier.package(),
        impl_scope.target_path,
        &function.name,
    );
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
    site.push_synthesized(function);
}

/// Walk a synthesized default-method `Function` and rewrite every
/// reference to a protocol type-param (`M`, `R`, …) into the
/// concrete `TypeExpr` the impl pinned. The substitution covers
/// param signatures, the return type, and every `TypeExpr` inside
/// the body: match arms' typed-binding patterns,
/// `(M, Option<ReplyTo<R>>)` receive-arm payloads, let-
/// binding annotations, closures, and so on.
///
/// Without this, a default body like `Process.run`'s
/// tuple envelope would carry bare `M`
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
/// `(M, Option<ReplyTo<R>>)` rewrite all the way down.
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
        TypeExpr::Tuple { elements, .. } => {
            for element in elements {
                substitute_named_in_type_expr(element, from, to);
            }
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
        Statement::Destructure { pattern, value, .. } => {
            substitute_named_in_pattern(pattern, from, to);
            substitute_named_in_expr(value, from, to);
        }
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
        Pattern::EnumTuple { elements, .. }
        | Pattern::Constructor { elements, .. }
        | Pattern::Tuple { elements, .. } => {
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
    site: &ConformanceSite<'_>,
    definition: &ProtocolDefinition,
    impl_scope: ProtocolImplScope<'_>,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let declared_functions = site.declared_functions();
    let declared: HashMap<&str, &Function> = declared_functions
        .iter()
        .map(|function| (function.name.as_str(), *function))
        .collect();
    let ProtocolImplScope {
        protocol_identifier,
        target_path,
        ..
    } = impl_scope;
    let context = site.context(protocol_identifier, &target_path.join("."));
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
                         ({context})",
                        method.name,
                    ),
                    site.span(),
                ));
            }
            None => {}
        }
    }
    // Extra-method strictness applies to impl blocks only. A type
    // body mixes protocol methods with fields and inherent functions
    // by design, while an `impl P for T` block widening the type's
    // public surface with non-protocol methods is rejected.
    let ConformanceSite::Impl(_) = site else {
        return;
    };
    let protocol_method_names: HashMap<&str, ()> = definition
        .methods
        .iter()
        .map(|m| (m.name.as_str(), ()))
        .collect();
    for function in &declared_functions {
        // Type-private helpers may live alongside the protocol
        // methods they support. Only public extras are rejected,
        // since they would silently widen the type's public surface
        // from inside a conformance block.
        if function.visibility == Visibility::Private {
            continue;
        }
        if !protocol_method_names.contains_key(function.name.as_str()) {
            diagnostics.push(Diagnostic::error_with_hint(
                format!(
                    "method `{}` is not declared in protocol `{protocol_identifier}` \
                     ({context})",
                    function.name,
                ),
                "mark it `priv fn` if it is an implementation helper, or declare it \
                 in the type's own body",
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
        protocol_identifier,
        protocol_subst,
        target_identifier,
        target_path,
        ..
    } = impl_scope;
    let method_identifier = Identifier::member(
        target_identifier.package(),
        target_path,
        &impl_function.name,
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
        let expected_ty = substitute(&want.ty, protocol_subst);
        if !types_equivalent(&expected_ty, &got.ty, registry) {
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
    if !types_equivalent(&expected_return, &actual.return_type, registry) {
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
