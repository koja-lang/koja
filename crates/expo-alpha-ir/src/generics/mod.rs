//! Generic monomorphization for the alpha IR pipeline.
//!
//! Generics are pipeline-internal scratch state: lowering produces
//! only concrete decls in [`crate::IRPackage`], and the typecheck
//! registry stays the single source of truth for generic-decl
//! shape. Discovery and specialization both live here.
//!
//! Two pieces:
//!
//! - [`Instantiation`] — one `(template, args)` pair recorded by
//!   [`crate::lower::package::resolved_type_to_ir_type`] every time
//!   it lowers a [`ResolvedType`] with non-empty `type_args`.
//! - [`instantiate`] — the worklist driver. Dedupes the per-package
//!   instantiation lists, and for each `(template, args)` pair:
//!   1. fetches the [`StructDefinition`] / [`EnumDefinition`] from
//!      the typecheck registry,
//!   2. substitutes the `args` into each declared field /
//!      payload-type [`ResolvedType`] via [`substitute_resolved_type`]
//!      (a local copy, intentionally — the typecheck variant lives
//!      in a different layer with a different invariant about
//!      Phantom slots),
//!   3. lowers the substituted shape into a concrete
//!      [`IRStructDecl`] / [`IREnumDecl`] via the same
//!      `resolved_type_to_ir_type` lowering helper used everywhere
//!      else, which may itself discover new instantiations,
//!   4. inserts the concrete decl into the owning [`IRPackage`].
//!
//! Step (3) is what makes the loop a worklist: lowering
//! `Pair<Box<Int>, String>`'s field types runs `Box<Int>` through
//! `resolved_type_to_ir_type` and discovers a fresh `Box<Int>`
//! instantiation. The driver loops until no new instantiations
//! appear.

mod monomorphize;

use std::collections::BTreeSet;

use expo_alpha_typecheck::GlobalRegistry;
use expo_ast::identifier::{GlobalRegistryId, Resolution, ResolvedType};

use crate::package::IRPackage;

/// One discovered generic instantiation. Recorded by
/// [`crate::lower::package::resolved_type_to_ir_type`] every time it
/// lowers a [`ResolvedType`] with a non-empty `type_args` list;
/// deduped by [`instantiate`] via [`BTreeSet`] before any
/// monomorphization runs.
#[derive(Debug, Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct Instantiation {
    pub(crate) template: GlobalRegistryId,
    pub(crate) args: Vec<ResolvedType>,
}

/// Dedupe `instantiations`, monomorphize each one against the
/// typecheck `registry`, and insert the resulting concrete decl
/// into the [`IRPackage`] in `packages` whose `package` label
/// matches the template's owning package. Loops until the
/// instantiation set stabilizes — a single round may surface fresh
/// instantiations (a generic field whose declared type is itself
/// a non-leaf generic instantiation).
///
/// Panics if an instantiation references a registry id with no
/// lifted definition or if no [`IRPackage`] in `packages` matches
/// the template's owner — both are lower invariant violations.
pub(crate) fn instantiate(
    instantiations: Vec<Instantiation>,
    registry: &GlobalRegistry,
    packages: &mut [IRPackage],
) {
    let mut done: BTreeSet<Instantiation> = BTreeSet::new();
    let mut worklist: Vec<Instantiation> = instantiations;
    while let Some(inst) = worklist.pop() {
        if !done.insert(inst.clone()) {
            continue;
        }
        let mut discovered: Vec<Instantiation> = Vec::new();
        monomorphize::monomorphize(&inst, registry, packages, &mut discovered);
        worklist.extend(discovered);
    }
}

/// Substitute `args` into `template`, replacing every leaf
/// [`Resolution::TypeParam { owner, .. }`][Resolution::TypeParam]
/// whose `owner` matches with `args[index]`. Other heads recurse
/// into their `type_args`.
///
/// Local copy of the typecheck-side substitution — the contract
/// here is "every Param leaf has a concrete arg" (enforced by the
/// inference-then-substitute flow at construction sites in
/// typecheck), so we panic on out-of-range index instead of
/// substituting to `ResolvedType::unresolved` like the typecheck
/// variant does for unresolved Phantom slots.
pub(crate) fn substitute_resolved_type(
    template: &ResolvedType,
    args: &[ResolvedType],
    owner: GlobalRegistryId,
) -> ResolvedType {
    if let Resolution::TypeParam {
        owner: param_owner,
        index,
    } = template.resolution
        && param_owner == owner
    {
        return args
            .get(index.as_u32() as usize)
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "alpha IR generics: TypeParam index {} out of range \
                     (owner `{owner}`, args.len() == {})",
                    index.as_u32(),
                    args.len(),
                );
            });
    }
    ResolvedType {
        resolution: template.resolution,
        type_args: template
            .type_args
            .iter()
            .map(|arg| substitute_resolved_type(arg, args, owner))
            .collect(),
    }
}
