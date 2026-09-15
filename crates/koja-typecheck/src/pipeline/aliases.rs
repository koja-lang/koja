//! File-private `alias Pkg.Name [as Local]` validation +
//! path-rewrite helper.
//!
//! Aliases bind a single `local_name` to a target [`Identifier`],
//! which names a type, a package-level function, or a package-level
//! constant. Use sites may project further segments through that
//! head. This module's [`rewrite_through_aliases`] does the
//! projection so the lift / resolve passes call one helper
//! regardless of path depth.
//!
//! Validation runs once between [`super::collect`] and
//! [`super::lift_signatures`] so every signature site sees a
//! validated alias roster. Diagnostics fire at the alias decl
//! itself. Alias *use* sites that don't resolve fall through to the
//! same "type not registered" diagnostic any other unknown name
//! would produce.

use std::collections::BTreeMap;

use koja_ast::ast::{AliasDecl, Diagnostic, Item, Name, name_texts, path_text};
use koja_ast::identifier::{GlobalRegistryId, Identifier};

use crate::pipeline::visibility::check_reference_visibility;
use crate::program::CheckedPackage;
use crate::registry::{GlobalKind, GlobalRegistry, RegistryEntry};

/// If `path[0]` matches an alias's `local_name`, resolve the alias's
/// target through reference precedence (current package, head-as-
/// package, then `Global`) and project the user's remaining segments
/// onto it. Returns `None` when no alias binds `path[0]`, or when the
/// target itself doesn't resolve (the validator reports the latter at
/// the alias decl).
///
/// Resolving the target with full precedence (rather than assuming
/// segment 0 is the package) is what lets an alias name a current-
/// package or `Global` nested type without spelling the package:
/// `alias Process.StopReason as StopReason` binds
/// `Global.Process.StopReason` from any package, mirroring how a bare
/// `Process.StopReason` reference resolves.
pub(crate) fn rewrite_through_aliases(
    aliases: &[AliasDecl],
    path: &[String],
    package: &str,
    registry: &GlobalRegistry,
) -> Option<Identifier> {
    let head = path.first()?;
    let alias = aliases.iter().find(|a| a.local_name == head.as_str())?;
    if alias.path.len() < 2 {
        return None;
    }
    let (_, entry) = lookup_alias_target(&alias.path, package, registry)?;
    let mut segments = entry.identifier.path().to_vec();
    segments.extend(path[1..].iter().cloned());
    Some(Identifier::new(entry.identifier.package(), segments))
}

/// Resolve an alias's *target* path to its registry entry using the
/// same precedence as ordinary type references: current package,
/// then head-as-package, then `Global`. No alias rewriting (a target
/// names a concrete type) and no single-segment primitive guard
/// (targets are always qualified). Shared by [`rewrite_through_aliases`]
/// and [`validate_file_aliases`] so use sites and the validator agree.
fn lookup_alias_target<'r>(
    target_path: &[Name],
    package: &str,
    registry: &'r GlobalRegistry,
) -> Option<(GlobalRegistryId, &'r RegistryEntry)> {
    let target_path = name_texts(target_path);
    if let Some(hit) = registry.lookup(&Identifier::new(package, target_path.clone())) {
        return Some(hit);
    }
    if target_path.len() >= 2
        && let Some(hit) =
            registry.lookup(&Identifier::new(&target_path[0], target_path[1..].to_vec()))
    {
        return Some(hit);
    }
    registry.lookup(&Identifier::new("Global", target_path))
}

/// Walk every file in `packages`, validating each [`AliasDecl`].
/// Runs after [`super::collect::collect_file_decls`] (so the registry
/// holds every package + Global decl) and before
/// [`super::lift_signatures::lift_signatures`] (so type-name
/// lookups in struct / fn signatures see the validated roster).
///
/// Seven checks per alias, each emitting one diagnostic and
/// continuing so the user sees every alias problem in one pass:
///
/// 1. Path length `>= 2`: alias targets must be qualified.
/// 2. Target identifier exists and names a type, or a package-level
///    function or constant. A member of a type is rejected.
/// 3. Local name has the target's case, lowercase for a function and
///    uppercase for everything else.
/// 4. Target is visible from the aliasing package (a `priv` decl
///    cannot be aliased cross-package).
/// 5. Local name not already used by another alias in this file.
/// 6. Local name doesn't shadow a current-package decl, *unless*
///    the alias's target is that very same identifier (redundant
///    self-alias is allowed, since the alias and the existing binding
///    resolve to the same id).
/// 7. Same shadow check against `Global`.
pub(crate) fn validate_aliases(
    packages: &[CheckedPackage],
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for pkg in packages {
        for file in &pkg.files {
            validate_file_aliases(file_alias_iter(file), &pkg.package, registry, diagnostics);
        }
    }
}

/// Collect every [`AliasDecl`] from a file's items, in source
/// order, as owned values. Lift's mutable passes (`lift_constant`,
/// `lift_impl`) iterate `&mut file.items` simultaneously with their
/// own alias scope, which would conflict with a `Vec<&AliasDecl>`
/// borrowed from the same file. Cloning is cheap (each AliasDecl
/// is two strings + a span) and decouples the alias slice from
/// the file's borrow lifetime.
pub(crate) fn collect_file_aliases(file: &koja_ast::ast::File) -> Vec<AliasDecl> {
    file_alias_iter(file).cloned().collect()
}

fn file_alias_iter(file: &koja_ast::ast::File) -> impl Iterator<Item = &AliasDecl> {
    file.items.iter().filter_map(|item| match item {
        Item::Alias(alias) => Some(alias),
        _ => None,
    })
}

fn validate_file_aliases<'a>(
    aliases: impl Iterator<Item = &'a AliasDecl>,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut seen_local_names: BTreeMap<String, koja_ast::span::Span> = BTreeMap::new();
    for alias in aliases {
        if !check_path_length(alias, diagnostics) {
            continue;
        }
        let Some((_, entry)) = lookup_alias_target(&alias.path, package, registry) else {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alias target `{}` is not a registered declaration",
                    path_text(&alias.path),
                ),
                alias.span,
            ));
            continue;
        };
        if !check_target_kind(alias, entry, diagnostics) {
            continue;
        }
        if !check_local_name_case(alias, entry, diagnostics) {
            continue;
        }
        check_reference_visibility(entry, package, alias.span, diagnostics);
        if !check_no_duplicate(alias, &mut seen_local_names, diagnostics) {
            continue;
        }
        check_no_shadow(alias, &entry.identifier, package, registry, diagnostics);
    }
}

fn check_path_length(alias: &AliasDecl, diagnostics: &mut Vec<Diagnostic>) -> bool {
    if alias.path.len() >= 2 {
        return true;
    }
    diagnostics.push(Diagnostic::error(
        format!(
            "alias path must be `Package.Name` (qualified), got `{}`",
            path_text(&alias.path),
        ),
        alias.span,
    ));
    false
}

/// Types alias at any depth. Functions and constants alias only at
/// package level, since a member of a type is reached through the
/// type and the type is what to alias.
fn check_target_kind(
    alias: &AliasDecl,
    entry: &RegistryEntry,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    match entry.kind {
        GlobalKind::Builtin(_)
        | GlobalKind::Enum(_)
        | GlobalKind::Protocol(_)
        | GlobalKind::Struct(_)
        | GlobalKind::TypeAlias(_) => true,
        GlobalKind::Constant(_) | GlobalKind::Function(_) => {
            let path = entry.identifier.path();
            if path.len() == 1 {
                return true;
            }
            let owner =
                Identifier::new(entry.identifier.package(), path[..path.len() - 1].to_vec());
            diagnostics.push(Diagnostic::error_with_hint(
                format!(
                    "alias target `{}` is a member of `{owner}`, not a package-level {}",
                    entry.identifier,
                    entry.kind.label(),
                ),
                format!("alias `{owner}` and call the member through it"),
                alias.span,
            ));
            false
        }
    }
}

/// A bare name's case tells the reader what it is. `bar(x)` calls a
/// function and `Bar` names a type or constant. An alias may not move
/// a name across that line.
fn check_local_name_case(
    alias: &AliasDecl,
    entry: &RegistryEntry,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let local = alias.local_name.as_str();
    let is_function = matches!(entry.kind, GlobalKind::Function(_));
    let starts_lower = local
        .chars()
        .next()
        .is_some_and(|c| c.is_lowercase() || c == '_');
    if is_function == starts_lower {
        return true;
    }
    let (want, fixed) = if is_function {
        ("lowercase", to_snake_case(local))
    } else {
        ("uppercase", to_pascal_case(local))
    };
    diagnostics.push(Diagnostic::error_with_hint(
        format!(
            "alias `{local}` names {} `{}`, so it must be {want}",
            entry.kind.label(),
            entry.identifier,
        ),
        format!(
            "write `alias {} as {fixed}`, or drop `as` to bind `{}`",
            path_text(&alias.path),
            entry.identifier.last(),
        ),
        alias.span,
    ));
    false
}

fn to_snake_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn to_pascal_case(name: &str) -> String {
    name.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}

fn check_no_duplicate(
    alias: &AliasDecl,
    seen: &mut BTreeMap<String, koja_ast::span::Span>,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    if let Some(prev_span) = seen.get(alias.local_name.as_str()) {
        diagnostics.push(
            Diagnostic::error(
                format!(
                    "duplicate alias `{}` because a local name can refer to only one declaration",
                    alias.local_name,
                ),
                alias.span,
            )
            .with_related(
                format!("the previous alias for `{}`", alias.local_name),
                *prev_span,
            ),
        );
        return false;
    }
    seen.insert(alias.local_name.text.clone(), alias.span);
    true
}

/// Reject any alias whose `local_name` collides with an existing
/// binding in the current package or `Global`. The pipeline treats
/// shadowing as a hard error. Carve-out: when the colliding
/// identifier *is* the alias target, the alias is redundant but
/// not a shadow (resolves to the same id). Allow it.
fn check_no_shadow(
    alias: &AliasDecl,
    target: &Identifier,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let local_name = alias.local_name.as_str();
    let scopes: [(&str, Identifier); 2] = [
        (package, Identifier::single(package, local_name)),
        ("Global", Identifier::single("Global", local_name)),
    ];
    for (label, candidate) in scopes {
        let Some((_, entry)) = registry.lookup(&candidate) else {
            continue;
        };
        if &entry.identifier == target {
            continue;
        }
        diagnostics.push(Diagnostic::error_with_hint(
            format!(
                "alias `{}` would shadow existing {} `{}` (the pipeline rejects shadowing)",
                alias.local_name,
                entry.kind.label(),
                entry.identifier,
            ),
            format!("rename the alias with `as` to avoid the collision in `{label}`"),
            alias.span,
        ));
        return;
    }
}
