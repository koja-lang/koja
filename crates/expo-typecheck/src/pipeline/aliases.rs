//! File-private `alias Pkg.Type [as Local]` validation +
//! path-rewrite helper.
//!
//! Aliases bind a single `local_name` to a target [`Identifier`].
//! Use sites may project further segments through that head; this
//! module's [`rewrite_through_aliases`] does the projection so the
//! lift / resolve passes call one helper regardless of path depth.
//!
//! Validation runs once between [`super::collect`] and
//! [`super::lift_signatures`] so every signature site sees a
//! validated alias roster. Diagnostics fire at the alias decl
//! itself; alias *use* sites that don't resolve fall through to the
//! same "type not registered" diagnostic any other unknown name
//! would produce.

use std::collections::BTreeMap;

use expo_ast::ast::{AliasDecl, Diagnostic, Item};
use expo_ast::identifier::Identifier;

use crate::program::CheckedPackage;
use crate::registry::{GlobalKind, GlobalRegistry};

/// If `path[0]` matches an alias's `local_name`, return the
/// rewritten target [`Identifier`]: alias target's package + (alias
/// target's tail segments concatenated with the user's remaining
/// segments). Returns `None` when no alias binds `path[0]`.
///
/// Decoupled from `path.len()` so alias machinery doesn't move when
/// nested types land — `O` (1 segment) and `O.Inner` (2 segments)
/// both project naturally through `alias Some.Outer as O`. Today
/// nested-target aliases simply fall through to "unknown type"
/// because the registry has no multi-segment entries; once nested-
/// type lifting lands they begin resolving without a code change
/// here.
pub(crate) fn rewrite_through_aliases(
    aliases: &[AliasDecl],
    path: &[String],
) -> Option<Identifier> {
    let head = path.first()?;
    let alias = aliases.iter().find(|a| a.local_name == *head)?;
    if alias.path.len() < 2 {
        return None;
    }
    let mut segments: Vec<String> = alias.path[1..].to_vec();
    segments.extend(path[1..].iter().cloned());
    Some(Identifier::new(&alias.path[0], segments))
}

/// Walk every file in `packages`, validating each [`AliasDecl`].
/// Runs after [`super::collect::collect_file_decls`] (so the registry
/// holds every package + Global decl) and before
/// [`super::lift_signatures::lift_signatures`] (so type-name
/// lookups in struct / fn signatures see the validated roster).
///
/// Five checks per alias, each emitting one diagnostic and
/// continuing so the user sees every alias problem in one pass:
///
/// 1. Path length `>= 2` — alias targets must be qualified.
/// 2. Target identifier exists and names a struct, enum, or
///    protocol (not a function or constant).
/// 3. Local name not already used by another alias in this file.
/// 4. Local name doesn't shadow a current-package decl, *unless*
///    the alias's target is that very same identifier (redundant
///    self-alias is allowed; the alias and the existing binding
///    resolve to the same id).
/// 5. Same shadow check against `Global`.
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
pub(crate) fn collect_file_aliases(file: &expo_ast::ast::File) -> Vec<AliasDecl> {
    file_alias_iter(file).cloned().collect()
}

fn file_alias_iter(file: &expo_ast::ast::File) -> impl Iterator<Item = &AliasDecl> {
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
    let mut seen_local_names: BTreeMap<String, expo_ast::span::Span> = BTreeMap::new();
    for alias in aliases {
        if !check_path_length(alias, diagnostics) {
            continue;
        }
        let Some(target) = build_target_identifier(alias) else {
            continue;
        };
        if !check_target_exists(alias, &target, registry, diagnostics) {
            continue;
        }
        if !check_no_duplicate(alias, &mut seen_local_names, diagnostics) {
            continue;
        }
        check_no_shadow(alias, &target, package, registry, diagnostics);
    }
}

fn check_path_length(alias: &AliasDecl, diagnostics: &mut Vec<Diagnostic>) -> bool {
    if alias.path.len() >= 2 {
        return true;
    }
    diagnostics.push(Diagnostic::error(
        format!(
            "alias path must be `Package.Type` (qualified), got `{}`",
            alias.path.join("."),
        ),
        alias.span,
    ));
    false
}

fn build_target_identifier(alias: &AliasDecl) -> Option<Identifier> {
    let (package, tail) = alias.path.split_first()?;
    if tail.is_empty() {
        return None;
    }
    Some(Identifier::new(package.as_str(), tail.to_vec()))
}

fn check_target_exists(
    alias: &AliasDecl,
    target: &Identifier,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let Some((_, entry)) = registry.lookup(target) else {
        diagnostics.push(Diagnostic::error(
            format!("alias target `{target}` is not a registered type"),
            alias.span,
        ));
        return false;
    };
    match entry.kind {
        GlobalKind::Enum(_)
        | GlobalKind::Protocol(_)
        | GlobalKind::Struct(_)
        | GlobalKind::TypeAlias(_) => true,
        GlobalKind::Constant(_) | GlobalKind::Function(_) => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alias target `{}` is a {}, not a struct, enum, or protocol",
                    entry.identifier,
                    entry.kind.label(),
                ),
                alias.span,
            ));
            false
        }
    }
}

fn check_no_duplicate(
    alias: &AliasDecl,
    seen: &mut BTreeMap<String, expo_ast::span::Span>,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    if let Some(prev_span) = seen.get(&alias.local_name) {
        diagnostics.push(Diagnostic::error_with_hint(
            format!(
                "duplicate alias `{}`: a local name can refer to only one type",
                alias.local_name,
            ),
            format!(
                "the previous alias for `{}` was at line {}",
                alias.local_name, prev_span.start.line,
            ),
            alias.span,
        ));
        return false;
    }
    seen.insert(alias.local_name.clone(), alias.span);
    true
}

/// Reject any alias whose `local_name` collides with an existing
/// binding in the current package or `Global` — the pipeline treats
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
    let local_name = alias.local_name.clone();
    let scopes: [(&str, Identifier); 2] = [
        (package, Identifier::new(package, vec![local_name.clone()])),
        (
            "Global",
            Identifier::new("Global", vec![local_name.clone()]),
        ),
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
                "alias `{}` would shadow existing {} `{}` -- the pipeline rejects shadowing",
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
