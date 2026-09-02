//! Retract derived `Equality` impls whose target cannot conform.
//!
//! `derive_equality` runs pre-collect on raw type expressions, so it
//! cannot know whether a field type such as `Option<fn (Int) -> Int>`
//! satisfies `Equality`. Lift knows, once every conformance fact is
//! recorded. A derived impl with a non-conforming field or payload is
//! removed here as if it had never been synthesized: the conformance
//! fact, the `equals?` registry entry, and the AST item all go. The
//! type then reads like `List<fn () -> Int>` does, and `==` on it
//! gets the "does not implement `Equality`" diagnostic instead of an
//! IR panic on the function payload.
//!
//! Retraction iterates to a fixpoint because one retraction can
//! invalidate a field of another derived type.

use koja_ast::ast::{File, ImplBlock, ImplOrigin, Item, TypeExpr};
use koja_ast::identifier::{GlobalRegistryId, Identifier, ResolvedType};

use crate::pipeline::collect::nominal_target_path;
use crate::program::CheckedPackage;
use crate::registry::{
    BoundOverlay, ConformanceScope, GlobalKind, GlobalRegistry, ResolvedProtocolBound,
    ResolvedVariantData,
};

const EQUALITY_PROTOCOL: &str = "Equality";
const EQ_METHOD: &str = "equals?";
const EQ_ARITY: usize = 2;

pub(super) fn retract_underivable_equality(
    packages: &mut [CheckedPackage],
    registry: &mut GlobalRegistry,
) {
    let equality = Identifier::new("Global", vec![EQUALITY_PROTOCOL.to_string()]);
    let Some((equality_id, _)) = registry.lookup(&equality) else {
        return;
    };
    loop {
        let mut retracted_any = false;
        for pkg in packages.iter_mut() {
            for file in &mut pkg.files {
                retracted_any |= retract_in_file(file, &pkg.package, equality_id, registry);
            }
        }
        if !retracted_any {
            return;
        }
    }
}

/// Remove every derived `Equality` impl in `file` whose target has a
/// non-conforming field. Returns whether anything was removed.
fn retract_in_file(
    file: &mut File,
    package: &str,
    equality_id: GlobalRegistryId,
    registry: &mut GlobalRegistry,
) -> bool {
    let targets: Vec<GlobalRegistryId> = file
        .items
        .iter()
        .filter_map(|item| underivable_target(item, package, equality_id, registry))
        .collect();
    if targets.is_empty() {
        return false;
    }
    for &target_id in &targets {
        let target = registry
            .get(target_id)
            .expect("retraction target was looked up above")
            .identifier
            .clone();
        let method = Identifier::member(target.package(), target.path(), EQ_METHOD);
        registry.remove_function(&method, EQ_ARITY);
        registry.remove_conformances(target_id, equality_id);
    }
    file.items.retain(|item| {
        !derived_equality_impl(item).is_some_and(|block| {
            impl_target_id(block, package, registry).is_some_and(|id| targets.contains(&id))
        })
    });
    true
}

/// The target id of a derived `Equality` impl whose field or payload
/// types do not all satisfy `Equality`, or `None` when the impl stays.
fn underivable_target(
    item: &Item,
    package: &str,
    equality_id: GlobalRegistryId,
    registry: &GlobalRegistry,
) -> Option<GlobalRegistryId> {
    let block = derived_equality_impl(item)?;
    let target_id = impl_target_id(block, package, registry)?;
    let overlay = parameterized_overlay(target_id, equality_id, registry);
    let bound = ResolvedProtocolBound {
        args: Vec::new(),
        protocol_id: equality_id,
    };
    let conforms = component_types(target_id, registry)
        .iter()
        .all(|ty| registry.bound_satisfied(ty, &bound, overlay.as_ref()));
    (!conforms).then_some(target_id)
}

fn derived_equality_impl(item: &Item) -> Option<&ImplBlock> {
    let Item::Impl(block) = item else {
        return None;
    };
    let is_equality = matches!(
        &block.trait_expr,
        TypeExpr::Named { path, .. } if path.len() == 1 && path[0] == EQUALITY_PROTOCOL
    );
    (block.origin == ImplOrigin::Derived && is_equality).then_some(block)
}

fn impl_target_id(
    block: &ImplBlock,
    package: &str,
    registry: &GlobalRegistry,
) -> Option<GlobalRegistryId> {
    let path = nominal_target_path(&block.target)?;
    registry
        .lookup_owner_path(path, package)
        .map(|(id, _, _)| id)
}

/// The impl's own `T: Equality` bounds as an overlay, so a generic
/// target's param-typed fields discharge through the condition the
/// derive attached rather than failing as bare params.
fn parameterized_overlay(
    target_id: GlobalRegistryId,
    equality_id: GlobalRegistryId,
    registry: &GlobalRegistry,
) -> Option<BoundOverlay> {
    registry
        .conformance_records(target_id, equality_id)?
        .iter()
        .find_map(|record| match &record.scope {
            ConformanceScope::Parameterized { bounds } => Some(BoundOverlay {
                bounds: bounds.clone(),
                owner: target_id,
            }),
            ConformanceScope::Concrete(_) => None,
        })
}

/// Every type the derived `equals?` body compares: struct fields, or
/// enum variant payloads. Builtins have no components.
fn component_types(target_id: GlobalRegistryId, registry: &GlobalRegistry) -> Vec<ResolvedType> {
    let Some(entry) = registry.get(target_id) else {
        return Vec::new();
    };
    match &entry.kind {
        GlobalKind::Struct(Some(def)) => def.fields.iter().map(|field| field.ty.clone()).collect(),
        GlobalKind::Enum(Some(def)) => def
            .variants
            .iter()
            .flat_map(|variant| match &variant.data {
                ResolvedVariantData::Struct(fields) => {
                    fields.iter().map(|field| field.ty.clone()).collect()
                }
                ResolvedVariantData::Tuple(types) => types.clone(),
                ResolvedVariantData::Unit => Vec::new(),
            })
            .collect(),
        _ => Vec::new(),
    }
}
