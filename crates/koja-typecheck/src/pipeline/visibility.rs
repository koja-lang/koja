//! Cross-phase enforcement of `priv` on non-call reference sites.
//!
//! Call sites enforce `priv fn` in `resolve::calls` (see
//! `check_callee_visibility` there). This module is the equivalent
//! seam for every other reference position. Those are type expressions
//! in signatures (`lift_signatures`), constructors / patterns / static
//! receivers (`resolve`), `extend` targets (`collect`), and `alias`
//! targets (`aliases`). It also owns the header leak checks,
//! [`check_signature_leaks`] for the types on a signature and
//! [`check_value_leaks`] for values such as field defaults.

use koja_ast::ast::{Diagnostic, Expr, ExprKind};
use koja_ast::identifier::{AnonymousKind, GlobalRegistryId, Identifier, Resolution, ResolvedType};
use koja_ast::span::Span;
use koja_ast::visit::{self, Visitor};

use crate::registry::{
    GlobalKind, GlobalRegistry, RegistryEntry, ResolvedVariantData, VisibilityScope,
};

/// Enforce a decl's [`VisibilityScope`] at a reference site. A
/// violation pushes one diagnostic and resolution proceeds, so
/// callers see exactly one error per offending site and downstream
/// passes walk a populated tree. Only `PackagePrivate` can fire
/// here. `TypePrivate` exists solely for functions, which are
/// gated at call sites.
pub(crate) fn check_reference_visibility(
    entry: &RegistryEntry,
    referrer_package: &str,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if entry.visibility != VisibilityScope::PackagePrivate
        || entry.identifier.package() == referrer_package
    {
        return;
    }
    diagnostics.push(
        Diagnostic::error_with_hint(
            format!(
                "private {} `{}` cannot be referenced from package `{referrer_package}`",
                entry.kind.label(),
                entry.identifier,
            ),
            format!(
                "`{}` is `priv`, usable only from package `{}`",
                entry.identifier,
                entry.identifier.package(),
            ),
            span,
        )
        .with_related("declared here", entry.name_span),
    );
}

/// Reject every public declaration whose signature surface mentions a
/// same-package `priv` type. Runs right after `lift_signatures`, so
/// every surface is a stamped [`ResolvedType`] with registry ids.
/// Cross-package private mentions are not re-checked here. The
/// reference gate already diagnosed them during lift. Field defaults
/// are the value half of this rule and resolve later, so
/// `resolve::field_defaults` runs [`check_value_leaks`] on them.
///
/// Functions declared on a private type are exempt. Their signatures
/// necessarily mention the type (`self`, constructors returning
/// `Self`), and the type itself is unreachable from other packages,
/// so nothing leaks. The exemption is function-only on purpose. A
/// public nested type under a private owner is still resolvable by
/// its full path, so its surface is checked like any other.
pub(crate) fn check_signature_leaks(registry: &GlobalRegistry, diagnostics: &mut Vec<Diagnostic>) {
    // Registry iteration order is unstable, so findings are keyed and
    // sorted before they join the diagnostic stream.
    let mut findings: Vec<(String, Diagnostic)> = Vec::new();
    for (_, entry) in registry.iter() {
        if entry.visibility != VisibilityScope::Public
            || is_function_on_private_type(entry, registry)
        {
            continue;
        }
        for leaked in leaked_private_mentions(entry, registry) {
            findings.push((
                entry.identifier.to_string(),
                Diagnostic::error_with_hint(
                    format!(
                        "public {} `{}` exposes private {} `{}`",
                        entry.kind.label(),
                        entry.identifier,
                        leaked.kind.label(),
                        leaked.identifier,
                    ),
                    format!(
                        "declare `{}` as `priv` too, or remove `priv` from `{}`",
                        entry.identifier.last(),
                        leaked.identifier.last(),
                    ),
                    entry.name_span,
                ),
            ));
        }
    }
    findings.sort_by(|a, b| a.0.cmp(&b.0));
    diagnostics.extend(findings.into_iter().map(|(_, diagnostic)| diagnostic));
}

/// Is `entry` a function registered under a private owner type
/// (`priv struct Hidden` with `fn make -> Hidden` inside)?
fn is_function_on_private_type(entry: &RegistryEntry, registry: &GlobalRegistry) -> bool {
    if !matches!(entry.kind, GlobalKind::Function(_)) {
        return false;
    }
    let path = entry.identifier.path();
    if path.len() < 2 {
        return false;
    }
    let owner = Identifier::new(entry.identifier.package(), path[..path.len() - 1].to_vec());
    registry
        .lookup(&owner)
        .is_some_and(|(_, owner_entry)| owner_entry.visibility == VisibilityScope::PackagePrivate)
}

/// Every same-package `priv` entry mentioned on `entry`'s signature
/// surface, deduped in surface order.
fn leaked_private_mentions<'r>(
    entry: &RegistryEntry,
    registry: &'r GlobalRegistry,
) -> Vec<&'r RegistryEntry> {
    let mut mentioned = Vec::new();
    collect_surface_ids(entry, &mut mentioned);
    same_package_private(&mentioned, entry.identifier.package(), registry)
}

/// The `priv` items of `package` among `ids`, deduped in order. A
/// cross-package private mention is not a leak here, since the
/// reference gate already diagnosed it.
fn same_package_private<'r>(
    ids: &[GlobalRegistryId],
    package: &str,
    registry: &'r GlobalRegistry,
) -> Vec<&'r RegistryEntry> {
    let mut leaked: Vec<&RegistryEntry> = Vec::new();
    for &id in ids {
        let Some(target) = registry.get(id) else {
            continue;
        };
        if target.visibility != VisibilityScope::PackagePrivate
            || target.identifier.package() != package
            || leaked
                .iter()
                .any(|seen| seen.identifier == target.identifier)
        {
            continue;
        }
        leaked.push(target);
    }
    leaked
}

/// Append every registry id mentioned on `entry`'s public surface,
/// meaning signature types per kind plus type-parameter bounds. Unstamped
/// (`None`) payloads contribute nothing, since collect already
/// diagnosed whatever prevented the stamp.
fn collect_surface_ids(entry: &RegistryEntry, ids: &mut Vec<GlobalRegistryId>) {
    match &entry.kind {
        GlobalKind::Constant(Some(definition)) => collect_type_ids(&definition.ty, ids),
        GlobalKind::Enum(Some(definition)) => {
            for variant in &definition.variants {
                match &variant.data {
                    ResolvedVariantData::Struct(fields) => {
                        for field in fields {
                            collect_type_ids(&field.ty, ids);
                        }
                    }
                    ResolvedVariantData::Tuple(types) => {
                        for ty in types {
                            collect_type_ids(ty, ids);
                        }
                    }
                    ResolvedVariantData::Unit => {}
                }
            }
        }
        GlobalKind::Function(definition) => {
            if let Some(signature) = &definition.signature {
                for param in &signature.params {
                    collect_type_ids(&param.ty, ids);
                }
                collect_type_ids(&signature.return_type, ids);
            }
        }
        GlobalKind::Protocol(Some(definition)) => {
            for method in &definition.methods {
                for param in &method.non_self_params {
                    collect_type_ids(&param.ty, ids);
                }
                collect_type_ids(&method.return_type, ids);
            }
        }
        GlobalKind::Struct(Some(definition)) => {
            for field in &definition.fields {
                collect_type_ids(&field.ty, ids);
            }
        }
        GlobalKind::TypeAlias(Some(expansion)) => collect_type_ids(expansion, ids),
        GlobalKind::Builtin(_)
        | GlobalKind::Constant(None)
        | GlobalKind::Enum(None)
        | GlobalKind::Protocol(None)
        | GlobalKind::Struct(None)
        | GlobalKind::TypeAlias(None) => {}
    }
    for bounds in &entry.type_param_bounds {
        for bound in bounds {
            ids.push(bound.protocol_id);
            for arg in &bound.args {
                collect_type_ids(arg, ids);
            }
        }
    }
}

/// Append every `Resolution::Global` id reachable in `ty`.
fn collect_type_ids(ty: &ResolvedType, ids: &mut Vec<GlobalRegistryId>) {
    match ty {
        ResolvedType::Anonymous(AnonymousKind::Function { params, ret }) => {
            for param in params {
                collect_type_ids(param, ids);
            }
            collect_type_ids(ret, ids);
        }
        ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) => {
            for element in elements {
                collect_type_ids(element, ids);
            }
        }
        ResolvedType::Named {
            resolution,
            type_args,
        } => {
            if let Resolution::Global(id) = resolution {
                ids.push(*id);
            }
            for arg in type_args {
                collect_type_ids(arg, ids);
            }
        }
        ResolvedType::Union(members) => {
            for member in members {
                collect_type_ids(member, ids);
            }
        }
        ResolvedType::Failed | ResolvedType::Unresolved => {}
    }
}

/// Reject every same-package `priv` item that `value`, a resolved
/// expression on `owner`'s header such as a field default, names. A
/// private owner exposes nothing. `subject` names the position in
/// the diagnostic, as in "default for field `x` of `Pkg.Config`".
///
/// Only package-private items can fire here. A cross-package or
/// type-private reference already failed when `value` resolved, and
/// a private type on the header is [`check_signature_leaks`]'s job.
pub(crate) fn check_value_leaks(
    value: &Expr,
    subject: &str,
    owner: &RegistryEntry,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if owner.visibility != VisibilityScope::Public {
        return;
    }
    let mut mentioned = Vec::new();
    collect_value_ids(value, &mut mentioned);
    for leaked in same_package_private(&mentioned, owner.identifier.package(), registry) {
        diagnostics.push(Diagnostic::error_with_hint(
            format!(
                "{subject} exposes private {} `{}`",
                leaked.kind.label(),
                leaked.identifier,
            ),
            format!(
                "`{}` is public, so its header can name only public items. Remove `priv` \
                 from `{}`, or declare `{}` as `priv` too",
                owner.identifier,
                leaked.identifier.last(),
                owner.identifier.last(),
            ),
            value.span,
        ));
    }
}

/// Append every `Resolution::Global` id that `value` names, in source
/// order. A constant read is an `Ident` whatever its spelling, and a
/// named function reference carries its target. No other expression
/// node names a global value.
fn collect_value_ids(value: &Expr, ids: &mut Vec<GlobalRegistryId>) {
    struct Collector<'a>(&'a mut Vec<GlobalRegistryId>);

    impl<'ast> Visitor<'ast> for Collector<'_> {
        fn visit_expr(&mut self, expr: &'ast Expr) {
            match &expr.kind {
                ExprKind::Ident {
                    resolution: Resolution::Global(id),
                    ..
                }
                | ExprKind::NamedFunctionReference {
                    target: Resolution::Global(id),
                    ..
                } => self.0.push(*id),
                _ => {}
            }
            visit::walk_expr(self, expr);
        }
    }

    Collector(ids).visit_expr(value);
}
