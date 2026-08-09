//! Surface-shape AST rewrites that ride alongside the typecheck
//! pipeline. Two phases:
//!
//! - [`derive_debug::derive_debug_package`] +
//!   [`derive_equality::derive_equality_package`] run
//!   **pre-collect** (called from [`crate::check_program`]) so the
//!   new `impl Debug for T` / `impl Equality for T` blocks land
//!   before name binding sees them.
//! - [`synthesize_program`] runs **post-lift** and only mutates
//!   function bodies (today: the `for` loop desugar in
//!   [`for_desugar`]). Item-introducing rewrites can't live here
//!   without re-running collect / lift on the new items.

pub(crate) mod derive_debug;
pub(crate) mod derive_equality;
mod for_desugar;

use koja_ast::ast::{File, Function, ImplMember, Item, TypeExpr};

use crate::program::CheckedPackage;

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

/// Apply every body-mutating synthesizer to every fn body across
/// `packages`. Item-introducing synthesizers (e.g.
/// [`derive_debug::derive_debug`]) run earlier in the pipeline. See
/// the module-level doc.
pub(crate) fn synthesize_program(packages: &mut [CheckedPackage]) {
    for pkg in packages.iter_mut() {
        for file in &mut pkg.files {
            for item in &mut file.items {
                synthesize_item(item);
            }
            if let Some(body) = file.body.as_mut() {
                let mut counter = SynthCounter::default();
                for_desugar::desugar_body(body, &mut counter);
            }
        }
    }
}

fn synthesize_item(item: &mut Item) {
    match item {
        Item::Function(function) => synthesize_function(function),
        Item::Builtin(decl) => {
            for function in &mut decl.functions {
                synthesize_function(function);
            }
        }
        Item::Struct(decl) => {
            for function in &mut decl.functions {
                synthesize_function(function);
            }
        }
        Item::Enum(decl) => {
            for function in &mut decl.functions {
                synthesize_function(function);
            }
        }
        Item::Impl(impl_block) => {
            for member in &mut impl_block.members {
                if let ImplMember::Function(function) = member {
                    synthesize_function(function);
                }
            }
        }
        Item::Extend(extend_block) => {
            for member in &mut extend_block.members {
                if let ImplMember::Function(function) = member {
                    synthesize_function(function);
                }
            }
        }
        Item::Alias(_) | Item::Constant(_) | Item::Protocol(_) | Item::TypeAlias(_) => {}
    }
}

fn synthesize_function(function: &mut Function) {
    if let Some(body) = function.body.as_mut() {
        let mut counter = SynthCounter::default();
        for_desugar::desugar_body(body, &mut counter);
    }
}

/// Per-function-fresh counter that hands out unique slot ids for
/// synthetic local names (e.g. `__it_<id>` from the `for` desugar).
#[derive(Default)]
pub(super) struct SynthCounter(u32);

impl SynthCounter {
    pub(super) fn next(&mut self) -> u32 {
        let id = self.0;
        self.0 = self
            .0
            .checked_add(1)
            .expect("synthesize: more than 2^32 synthetic slots in one function");
        id
    }
}
