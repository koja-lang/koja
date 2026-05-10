//! Surface-shape AST rewrites between `lift_signatures` and
//! `resolve`. Today: the `for` loop desugar in [`for_desugar`].
//! Future synthesizers (default `Debug` impl, async desugar, …)
//! land as additional submodules.
//!
//! The list-literal rewrite lives in [`super::resolve::expr`]
//! instead — `[a, b, c]` desugars to `List.new().append(a)...`
//! with the chain's resolutions stamped from the elements' inferred
//! types, which is only possible after resolve has seen the
//! element expressions.

mod for_desugar;

use expo_ast::ast::{Function, ImplMember, Item};

use crate::program::CheckedPackage;

/// Apply every registered synthesizer to every fn body across
/// `packages`.
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
