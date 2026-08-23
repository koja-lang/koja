//! Derived conformance synthesis before declaration collection.

pub(crate) mod derive_debug;
pub(crate) mod derive_equality;

use koja_ast::ast::{File, Item, TypeExpr};

/// Type paths whose conformance header lists `protocol` by leaf
/// name (`Debug` in `struct T: Debug`). The derive passes treat a
/// header entry like a hand-written impl and skip synthesis.
pub(super) fn header_conformance_targets(file: &File, protocol: &str) -> Vec<String> {
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
                .then(|| path.join("."))
        })
        .collect()
}

fn conformance_head(entry: &TypeExpr) -> Option<&str> {
    match entry {
        TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. } => {
            path.last().map(String::as_str)
        }
        _ => None,
    }
}
