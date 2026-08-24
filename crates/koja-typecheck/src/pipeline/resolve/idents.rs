//! Bare identifier, qualified member, and `self` resolution.

use koja_ast::ast::{Diagnostic, Expr, ExprKind};
use koja_ast::identifier::{AnonymousKind, Identifier, LocalId, Resolution, ResolvedType};
use koja_ast::span::Span;

use crate::pipeline::visibility::check_reference_visibility;
use crate::registry::{
    FunctionLookup, FunctionSignature, GlobalKind, GlobalRegistry, RegistryEntry, VisibilityScope,
};

use super::calls::closest_arity;
use super::ctx::Resolver;
use super::paths::{PackageMember, lookup_package_member, static_dotted_path};
use super::types::lookup_type;

/// Resolve a bare identifier expression. Locals win first. Package-
/// level constants resolve through a global lookup so an
/// `EARTH_RADIUS` reference at a use site stamps `Resolution::Global`
/// and returns the constant's stamped type, with auto-imported
/// `Global` constants (`STDOUT`) as the fallback when the current
/// package has no match. Named global functions require explicit
/// `&name/arity` syntax. (The static-method receiver and
/// `Type.method(...)` call paths each handle struct-name resolution
/// directly so they do not go through this helper.)
pub(super) fn resolve_ident(
    name: &str,
    resolution: &mut Resolution,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if let Some((local_id, ty)) = resolver.scope.lookup(name) {
        *resolution = Resolution::Local(local_id);
        return ty.clone();
    }
    let global_id = Identifier::new(resolver.package, vec![name.to_string()]);
    if let Some((id, entry)) = resolver.registry.lookup(&global_id) {
        match &entry.kind {
            GlobalKind::Constant(Some(def)) => {
                *resolution = Resolution::Global(id);
                return def.ty.clone();
            }
            GlobalKind::Function(_) => {
                diagnose_explicit_reference(name, &global_id, resolver.registry, span, diagnostics);
                return ResolvedType::unresolved();
            }
            _ => {}
        }
    }
    let fallback = Identifier::new("Global", vec![name.to_string()]);
    if let Some((id, entry)) = resolver.registry.lookup(&fallback)
        && let GlobalKind::Constant(Some(def)) = &entry.kind
    {
        *resolution = Resolution::Global(id);
        return def.ty.clone();
    }
    diagnostics.push(Diagnostic::error(
        format!("unknown identifier `{name}` in this scope"),
        span,
    ));
    ResolvedType::unresolved()
}

/// Resolve a package-qualified member read: a constant
/// (`Pkg.MAX_SIZE`) or a function that needs explicit reference syntax
/// (`Pkg.helper`). The
/// capitalized form parses as a unit enum construction and the
/// lowercase form as a field access, so neither shape reaches
/// [`resolve_ident`]. When the dotted path names a constant or a
/// function, the node is rewritten to an identifier with a stamped
/// `Resolution::Global` so IR lowering reads it through the same
/// path as a bare name. Locals and types win over package prefixes
/// and bail to the normal resolution paths. Members are top-level
/// declarations, so only two-segment paths apply. Deeper chains like
/// `Pkg.ORIGIN.x` fall through and resolve their prefix recursively.
pub(super) fn resolve_qualified_member(
    expr: &mut Expr,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ResolvedType> {
    let path = static_dotted_path(&expr.kind)?;
    let [package, name] = path.as_slice() else {
        return None;
    };
    let (id, entry) = match lookup_package_member(package, name, resolver) {
        PackageMember::Found(id, entry) => (id, entry),
        PackageMember::NotAPackage => return None,
        PackageMember::UnknownMember => {
            diagnostics.push(Diagnostic::error(
                format!("package `{package}` has no constant or function `{name}`"),
                expr.span,
            ));
            return Some(ResolvedType::unresolved());
        }
    };
    let ty = match &entry.kind {
        GlobalKind::Constant(Some(definition)) => definition.ty.clone(),
        GlobalKind::Function(_) => {
            let label = format!("{package}.{name}");
            diagnose_explicit_reference(
                &label,
                &entry.identifier,
                resolver.registry,
                expr.span,
                diagnostics,
            );
            return Some(ResolvedType::unresolved());
        }
        GlobalKind::Constant(None) => panic!(
            "resolve_qualified_member found `{}` without a stamped definition. \
             lifting runs before body resolution",
            entry.identifier,
        ),
        other => {
            diagnostics.push(Diagnostic::error(
                format!("`{package}.{name}` is a {}, not a value", other.label()),
                expr.span,
            ));
            return Some(ResolvedType::unresolved());
        }
    };
    check_reference_visibility(entry, resolver.package, expr.span, diagnostics);
    expr.kind = ExprKind::Ident {
        name: path.join("."),
        resolution: Resolution::Global(id),
    };
    Some(ty)
}

pub(super) fn resolve_named_function_reference(
    path: &[String],
    arity: usize,
    target: &mut Resolution,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let label = path.join(".");
    if path.is_empty() {
        diagnostics.push(Diagnostic::error(
            "named function reference has no function name",
            span,
        ));
        return ResolvedType::unresolved();
    }

    if resolver.scope.lookup(&path[0]).is_some() {
        diagnostics.push(Diagnostic::error_with_hint(
            format!("`&{label}/{arity}` cannot bind a local receiver or local function"),
            "use the local function value directly, or wrap a receiver call in a closure",
            span,
        ));
        return ResolvedType::unresolved();
    }

    let selected = if path.len() == 1 {
        resolve_bare_reference(path, arity, resolver, span, diagnostics)
    } else {
        resolve_qualified_reference(path, arity, resolver, span, diagnostics)
    };
    let Some((id, entry)) = selected else {
        return ResolvedType::unresolved();
    };

    check_function_reference_visibility(entry, resolver, span, diagnostics);
    if !entry.type_params.is_empty() {
        diagnostics.push(Diagnostic::error_with_hint(
            format!("cannot reference generic function `{label}` directly"),
            "wrap the call in a closure so its type arguments can be inferred",
            span,
        ));
        return ResolvedType::unresolved();
    }
    let signature = entry.expect_function_signature();
    *target = Resolution::Global(id);
    function_value_type(signature)
}

fn resolve_bare_reference<'a>(
    path: &[String],
    arity: usize,
    resolver: &'a Resolver<'_>,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(koja_ast::identifier::GlobalRegistryId, &'a RegistryEntry)> {
    let name = &path[0];
    if let Some(enclosing) = resolver.enclosing_type {
        let identifier = Identifier::member(resolver.package, enclosing, name);
        match resolver.registry.function_lookup(&identifier, arity) {
            FunctionLookup::Found(id, entry) => return Some((id, entry)),
            FunctionLookup::WrongArity(arities) => {
                diagnose_wrong_arity_reference(
                    &identifier,
                    name,
                    arity,
                    &arities,
                    span,
                    diagnostics,
                );
                return None;
            }
            FunctionLookup::NoFunctions => {
                if let Some((_, entry)) = resolver.registry.lookup(&identifier) {
                    diagnose_non_function_reference(name, arity, entry, span, diagnostics);
                    return None;
                }
            }
        }
    }
    let identifier = Identifier::new(resolver.package, path.to_vec());
    match resolver.registry.function_lookup(&identifier, arity) {
        FunctionLookup::Found(id, entry) => Some((id, entry)),
        FunctionLookup::WrongArity(arities) => {
            diagnose_wrong_arity_reference(&identifier, name, arity, &arities, span, diagnostics);
            None
        }
        FunctionLookup::NoFunctions => {
            match resolver.registry.lookup(&identifier) {
                Some((_, entry)) => {
                    diagnose_non_function_reference(name, arity, entry, span, diagnostics)
                }
                None => diagnostics.push(Diagnostic::error(
                    format!("unknown named function reference `&{name}/{arity}`"),
                    span,
                )),
            }
            None
        }
    }
}

fn resolve_qualified_reference<'a>(
    path: &[String],
    arity: usize,
    resolver: &'a Resolver<'_>,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(koja_ast::identifier::GlobalRegistryId, &'a RegistryEntry)> {
    let member = path.last().expect("qualified path is non-empty");
    let label = path.join(".");
    let owner_path = &path[..path.len() - 1];
    if let Some((_, owner)) = lookup_type(owner_path, resolver.resolution_scope()) {
        check_reference_visibility(owner, resolver.package, span, diagnostics);
        if !owner.type_params.is_empty() && !matches!(owner.kind, GlobalKind::Protocol(_)) {
            diagnostics.push(Diagnostic::error_with_hint(
                format!(
                    "cannot reference a function on generic type `{}` directly",
                    owner.identifier
                ),
                "wrap the call in a closure so the type arguments can be inferred",
                span,
            ));
            return None;
        }
        let identifier =
            Identifier::member(owner.identifier.package(), owner.identifier.path(), member);
        match resolver.registry.function_lookup(&identifier, arity) {
            FunctionLookup::Found(id, entry) => return Some((id, entry)),
            FunctionLookup::WrongArity(arities) => {
                diagnose_wrong_arity_reference(
                    &identifier,
                    &label,
                    arity,
                    &arities,
                    span,
                    diagnostics,
                );
                return None;
            }
            FunctionLookup::NoFunctions => {}
        }
        if let GlobalKind::Protocol(Some(definition)) = &owner.kind
            && definition
                .methods
                .iter()
                .any(|method| method.name == *member && method.arity == arity)
        {
            diagnostics.push(Diagnostic::error_with_hint(
                format!(
                    "cannot reference abstract protocol function `{}.{member}/{arity}`",
                    owner.identifier
                ),
                "reference a concrete implementation, or wrap a bounded call in a closure",
                span,
            ));
            return None;
        }
        match resolver.registry.lookup(&identifier) {
            Some((_, entry)) => {
                diagnose_non_function_reference(&label, arity, entry, span, diagnostics)
            }
            None => diagnostics.push(Diagnostic::error(
                format!(
                    "type `{}` has no function `{member}` with arity {arity}",
                    owner.identifier
                ),
                span,
            )),
        }
        return None;
    }

    if path.len() == 2 {
        let identifier = Identifier::new(&path[0], vec![member.clone()]);
        if resolver.registry.iter_in_package(&path[0]).next().is_some() {
            return match resolver.registry.function_lookup(&identifier, arity) {
                FunctionLookup::Found(id, entry) => Some((id, entry)),
                FunctionLookup::WrongArity(arities) => {
                    diagnose_wrong_arity_reference(
                        &identifier,
                        &label,
                        arity,
                        &arities,
                        span,
                        diagnostics,
                    );
                    None
                }
                FunctionLookup::NoFunctions => {
                    match resolver.registry.lookup(&identifier) {
                        Some((_, entry)) => {
                            diagnose_non_function_reference(&label, arity, entry, span, diagnostics)
                        }
                        None => diagnostics.push(Diagnostic::error(
                            format!(
                                "package `{}` has no function `{member}` with arity {arity}",
                                path[0]
                            ),
                            span,
                        )),
                    }
                    None
                }
            };
        }
    }

    diagnostics.push(Diagnostic::error(
        format!(
            "unknown named function reference `&{}/{arity}`",
            path.join(".")
        ),
        span,
    ));
    None
}

/// Diagnose `&label/arity` when functions exist under the name but
/// none at the requested arity.
fn diagnose_wrong_arity_reference(
    identifier: &Identifier,
    label: &str,
    arity: usize,
    arities: &[usize],
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnostics.push(Diagnostic::error_with_hint(
        format!("function `{identifier}` has no arity {arity}"),
        format!("did you mean `&{label}/{}`?", closest_arity(arities, arity)),
        span,
    ));
}

/// Diagnose `&label/arity` when the name resolves to a non-function
/// declaration (a struct, constant, nested type, ...).
fn diagnose_non_function_reference(
    label: &str,
    arity: usize,
    entry: &RegistryEntry,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnostics.push(Diagnostic::error(
        format!(
            "`&{label}/{arity}` does not name a function because `{}` is a {}",
            entry.identifier,
            entry.kind.label(),
        ),
        span,
    ));
}

fn diagnose_explicit_reference(
    label: &str,
    identifier: &Identifier,
    registry: &GlobalRegistry,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let arities = registry.function_arities(identifier);
    let example = arities.first().copied().unwrap_or(0);
    diagnostics.push(Diagnostic::error_with_hint(
        format!("named function `{label}` requires an explicit reference"),
        format!("write `&{label}/{example}` to select its arity"),
        span,
    ));
}

fn check_function_reference_visibility(
    entry: &RegistryEntry,
    resolver: &Resolver<'_>,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match entry.visibility {
        VisibilityScope::Public => {}
        VisibilityScope::PackagePrivate => {
            check_reference_visibility(entry, resolver.package, span, diagnostics);
        }
        VisibilityScope::TypePrivate(owner) if resolver.enclosing_type_id == Some(owner) => {}
        VisibilityScope::TypePrivate(owner) => {
            let owner_label = resolver
                .registry
                .get(owner)
                .map(|owner| owner.identifier.to_string())
                .unwrap_or_else(|| "<unknown>".to_string());
            diagnostics.push(Diagnostic::error_with_hint(
                format!(
                    "private function `{}` cannot be referenced from here",
                    entry.identifier
                ),
                format!(
                    "`{}` is `priv fn`, usable only from functions on `{owner_label}`",
                    entry.identifier
                ),
                span,
            ));
        }
    }
}

fn function_value_type(signature: &FunctionSignature) -> ResolvedType {
    ResolvedType::Anonymous(AnonymousKind::Function {
        params: signature.params.iter().map(|p| p.ty.clone()).collect(),
        ret: Box::new(signature.return_type.clone()),
    })
}

/// Resolve a `self` keyword expression. `self` is bound by the
/// enclosing instance method's `Param::Self_`, which the walker
/// seeds into the [`Resolver`]'s [`LocalScope`] under the name
/// `"self"`. A hit returns the receiver's struct type and stamps the
/// AST node's `local_id` slot so IR lower can read the slot through
/// the same `LocalRead` path body-declared locals use. A miss surfaces
/// as a diagnostic: `self` outside an instance method is invalid.
///
/// Note: `expr.resolution` keeps the receiver's *struct type* (not a
/// `Resolution::Local`). The `local_id` slot is the binding info,
/// the resolution slot is the static type. Same split as `ExprKind::Ident`,
/// where the inner `resolution` names the binding and the outer
/// `expr.resolution` carries the type.
///
/// [`LocalScope`]: crate::pipeline::local_scope::LocalScope
pub(super) fn resolve_self(
    local_id_slot: &mut Option<LocalId>,
    span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if let Some((local_id, ty)) = resolver.scope.lookup("self") {
        *local_id_slot = Some(local_id);
        return ty.clone();
    }
    diagnostics.push(Diagnostic::error(
        "`self` is only valid inside instance methods",
        span,
    ));
    ResolvedType::unresolved()
}
