//! Derived conformance synthesis before declaration collection.
//!
//! [`derive_protocol`] is the driver both derive passes share. It
//! scans a package for existing impls of one protocol, then appends
//! a synthesized impl block for every struct and enum without one.
//! The per-protocol modules supply only the body builders.

pub(crate) mod derive_debug;
pub(crate) mod derive_equality;

use koja_ast::ast::{
    EnumDecl, Expr, ExprKind, File, ImplBlock, Item, Name, StructDecl, TypeExpr, TypeParam,
    name_texts, path_text,
};
use koja_ast::identifier::Resolution;
use koja_ast::span::Span;

use crate::program::CheckedPackage;

/// The leaf name of a conformance or trait entry (`Debug` in
/// `struct T: Debug` or `impl Debug for T`). `None` for shapes that
/// cannot name a protocol.
fn conformance_head(entry: &TypeExpr) -> Option<&str> {
    match entry {
        TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. } => {
            path.last().map(Name::as_str)
        }
        _ => None,
    }
}

/// Append a derived `impl <protocol> for T` for each user struct and
/// enum in `pkg` that has no impl of `protocol` anywhere in the
/// package. A hand-written impl or a conformance header entry in
/// one file suppresses synthesis in every other file of the same
/// package. Synthesized targets join the existing set too, so a
/// type declared twice gets one derived impl and collect reports
/// the duplicate once.
pub(super) fn derive_protocol(
    pkg: &mut CheckedPackage,
    protocol: &str,
    struct_impl: fn(&StructDecl) -> Item,
    enum_impl: fn(&EnumDecl) -> Item,
) {
    let mut existing = package_impl_targets(pkg, protocol);
    for file in &mut pkg.files {
        synthesize_into_file(file, &mut existing, struct_impl, enum_impl);
    }
}

/// Whether `existing` (dotted impl targets) already covers the decl
/// at `path`.
fn has_impl(existing: &[String], path: &[Name]) -> bool {
    existing.contains(&name_texts(path).join("."))
}

/// Type paths whose conformance header lists `protocol` by leaf
/// name (`Debug` in `struct T: Debug`). A header entry counts like
/// a hand-written impl and skips synthesis.
fn header_conformance_targets(file: &File, protocol: &str) -> Vec<String> {
    file.items
        .iter()
        .filter_map(|item| {
            let (path, conformances) = match item {
                Item::Enum(decl) => (&decl.path, &decl.conformances),
                Item::Struct(decl) => (&decl.path, &decl.conformances),
                _ => return None,
            };
            conformances
                .iter()
                .any(|entry| conformance_head(entry) == Some(protocol))
                .then(|| name_texts(path).join("."))
        })
        .collect()
}

pub(super) fn ident_expr(name: &str, span: Span) -> Expr {
    Expr::new(
        ExprKind::Ident {
            name: name.to_string(),
            resolution: Resolution::Unresolved,
        },
        span,
    )
}

/// The dotted target path of an `impl <protocol> for T` block, or
/// `None` when the block implements another protocol. Generic args
/// are ignored so `impl Debug for List<T>` matches a struct named
/// `List`.
fn impl_target(block: &ImplBlock, protocol: &str) -> Option<String> {
    (conformance_head(&block.trait_expr) == Some(protocol))
        .then(|| type_expr_path(&block.target))
        .flatten()
}

pub(super) fn named_type(name: &str, span: Span) -> TypeExpr {
    TypeExpr::named(vec![Name::new(name, span)], span)
}

/// Empty enums (no variants) are uninhabited. A `match self end`
/// body with no arms is rejected by typecheck, and the type has no
/// value to format or compare anyway. Skip them.
fn needs_enum_derive(decl: &EnumDecl, existing: &[String]) -> bool {
    !decl.variants.is_empty() && !has_impl(existing, &decl.path)
}

fn needs_struct_derive(decl: &StructDecl, existing: &[String]) -> bool {
    !has_impl(existing, &decl.path)
}

/// Every type path in `pkg` that already conforms to `protocol`,
/// through a hand-written impl block or a conformance header.
fn package_impl_targets(pkg: &CheckedPackage, protocol: &str) -> Vec<String> {
    pkg.files
        .iter()
        .flat_map(|file| {
            let impls = file.items.iter().filter_map(|item| match item {
                Item::Impl(block) => impl_target(block, protocol),
                _ => None,
            });
            impls.chain(header_conformance_targets(file, protocol))
        })
        .collect()
}

pub(super) fn self_expr(span: Span) -> Expr {
    Expr::new(ExprKind::Self_ { local_id: None }, span)
}

/// Builds the `Target<Params>` type expression on the `impl ... for`
/// side, mirroring the type's own generic parameters so the impl
/// monomorphizes per concrete instantiation.
pub(super) fn self_target_type(path: &[Name], type_params: &[TypeParam], span: Span) -> TypeExpr {
    let path = synthetic_path(path, span);
    if type_params.is_empty() {
        TypeExpr::named(path, span)
    } else {
        let args = type_params
            .iter()
            .map(|tp| named_type(tp.name.as_str(), span))
            .collect();
        TypeExpr::generic(path, args, span)
    }
}

/// Append one synthesized impl per struct and enum in `file` that
/// `existing` does not cover, and record each new target in
/// `existing`.
fn synthesize_into_file(
    file: &mut File,
    existing: &mut Vec<String>,
    struct_impl: fn(&StructDecl) -> Item,
    enum_impl: fn(&EnumDecl) -> Item,
) {
    let mut synthesized: Vec<Item> = Vec::new();
    for item in &file.items {
        match item {
            Item::Struct(decl) if needs_struct_derive(decl, existing) => {
                synthesized.push(struct_impl(decl));
                existing.push(name_texts(&decl.path).join("."));
            }
            Item::Enum(decl) if needs_enum_derive(decl, existing) => {
                synthesized.push(enum_impl(decl));
                existing.push(name_texts(&decl.path).join("."));
            }
            _ => {}
        }
    }
    file.items.extend(synthesized);
}

/// Copy a declaration path's segments onto a synthesized node at
/// `span`.
pub(super) fn synthetic_path(path: &[Name], span: Span) -> Vec<Name> {
    path.iter()
        .map(|segment| Name::new(segment.as_str(), span))
        .collect()
}

/// The target type's full dotted path (`Net.TCPSocket`,
/// `Process.ExitSignal`), matched against a decl's
/// [`StructDecl::path`] / [`EnumDecl::path`].
fn type_expr_path(te: &TypeExpr) -> Option<String> {
    match te {
        TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. } => Some(path_text(path)),
        _ => None,
    }
}
