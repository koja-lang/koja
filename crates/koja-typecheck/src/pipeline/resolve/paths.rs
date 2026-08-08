//! Dotted static paths and package-qualified member lookup, shared
//! by static method dispatch, package function calls, and package
//! constant reads.

use koja_ast::ast::{EnumConstructionData, ExprKind};
use koja_ast::identifier::{GlobalRegistryId, Identifier};

use crate::registry::RegistryEntry;

use super::ctx::Resolver;
use super::types::lookup_type;

/// Collapse an expression to its dotted name path when one of the
/// static shapes matches. Returns:
///
/// - `Some(["Color"])` for bare `Ident("Color")`.
/// - `Some(["Crypto", "SHA256"])` for the parser's
///   `EnumConstruction { type_path: ["Crypto"], variant: "SHA256",
///   data: Unit }` shape, what `Crypto.SHA256.digest(...)` and
///   `HTTP.Headers.new()` parse to before disambiguation.
/// - `Some(["HTTP", "Headers"])` for an `Ident`-rooted
///   `FieldAccess` chain `FieldAccess { receiver: Ident("HTTP"),
///   field: "Headers" }`.
/// - `None` for everything else (value receivers, parenthesized
///   expressions, calls, etc.). Those flow through the value paths.
pub(super) fn static_dotted_path(kind: &ExprKind) -> Option<Vec<String>> {
    match kind {
        ExprKind::EnumConstruction {
            data: EnumConstructionData::Unit,
            type_path,
            variant,
        } => {
            let mut path = type_path.clone();
            path.push(variant.clone());
            Some(path)
        }
        ExprKind::Ident { .. } | ExprKind::FieldAccess { .. } => {
            let mut path = Vec::new();
            walk_dotted_path(kind, &mut path)?;
            Some(path)
        }
        _ => None,
    }
}

fn walk_dotted_path(kind: &ExprKind, out: &mut Vec<String>) -> Option<()> {
    match kind {
        ExprKind::Ident { name, .. } => {
            out.push(name.clone());
            Some(())
        }
        ExprKind::FieldAccess { receiver, field } => {
            walk_dotted_path(&receiver.kind, out)?;
            out.push(field.clone());
            Some(())
        }
        _ => None,
    }
}

/// Outcome of a `Pkg.member` lookup.
pub(super) enum PackageMember<'a> {
    /// `Pkg.member` names a registered declaration.
    Found(GlobalRegistryId, &'a RegistryEntry),
    /// The head is not a package reference here. Locals and types in
    /// scope shadow package names, and heads that name nothing fall
    /// through to the generic diagnostics.
    NotAPackage,
    /// The package has declarations, but none named `member`. Callers
    /// diagnose with their own expected-kind wording.
    UnknownMember,
}

/// Look up `Pkg.member` under the shadowing rules shared by
/// package-qualified function calls and constant reads.
pub(super) fn lookup_package_member<'a>(
    package: &str,
    member: &str,
    resolver: &Resolver<'a>,
) -> PackageMember<'a> {
    if resolver.scope.lookup(package).is_some() {
        return PackageMember::NotAPackage;
    }
    if lookup_type(&[package.to_string()], resolver.resolution_scope()).is_some() {
        return PackageMember::NotAPackage;
    }
    let target = Identifier::new(package, vec![member.to_string()]);
    if let Some((id, entry)) = resolver.registry.lookup(&target) {
        return PackageMember::Found(id, entry);
    }
    if resolver.registry.iter_in_package(package).next().is_some() {
        return PackageMember::UnknownMember;
    }
    PackageMember::NotAPackage
}
