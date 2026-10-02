//! Conformance records and bound discharge on [`GlobalRegistry`].
//!
//! A conformance fact (`T : P`) lives on the target entry's
//! definition, keyed by protocol id. This block records those facts,
//! finds the record that covers one instantiation, and decides
//! whether a type discharges a `ty: protocol` obligation. Tuples,
//! function types, and anonymous unions have no impl target, so they
//! discharge structurally.

use std::collections::BTreeMap;

use koja_ast::identifier::{
    AnonymousKind, GlobalRegistryId, Identifier, Resolution, ResolvedType, TypeParamIndex,
};

use super::{
    BoundOverlay, Conformance, ConformanceScope, GlobalKind, GlobalRegistry, ResolvedProtocolBound,
    UNIVERSAL_PROTOCOLS,
};
use crate::pipeline::resolve::types::types_equivalent;

/// The per-protocol conformance records a target entry carries.
type ConformanceMap = BTreeMap<GlobalRegistryId, Vec<Conformance>>;

impl GlobalRegistry {
    /// Record a [`Conformance`] of `target_id` to `protocol_id`.
    /// Returns the previously-recorded record when the new one
    /// overlaps it (a `Parameterized` record overlaps everything,
    /// two `Concrete` records overlap when their target args are
    /// equivalent). The caller emits the "duplicate `impl P for T`"
    /// diagnostic. Panics unless `target_id` names a builtin or a
    /// struct/enum with a stamped definition (lift orders
    /// enum/struct definition stamping before impl conformance
    /// recording).
    pub(crate) fn record_conformance(
        &mut self,
        target_id: GlobalRegistryId,
        protocol_id: GlobalRegistryId,
        conformance: Conformance,
    ) -> Option<Conformance> {
        // Overlap check first, since type equivalence needs `&self`
        // while the insert below holds the entry mutably.
        if let Some(existing) = self
            .conformance_records(target_id, protocol_id)
            .unwrap_or_default()
            .iter()
            .find(|record| self.conformances_overlap(record, &conformance))
        {
            return Some(existing.clone());
        }
        let entry = self.entries.get_mut(&target_id).unwrap_or_else(|| {
            panic!(
                "record_conformance on missing registry id {target_id}. This is a \
                 lift invariant violation",
            )
        });
        let Some(conformances) = conformance_map_mut(&mut entry.kind) else {
            panic!(
                "record_conformance on `{}` ({}). Only builtin and stamped \
                 struct/enum entries accept conformances",
                entry.identifier,
                entry.kind.label(),
            )
        };
        conformances
            .entry(protocol_id)
            .or_default()
            .push(conformance);
        None
    }

    /// The conformance record of `target_id` to `protocol_id` that
    /// covers the `target_args` instantiation. `Concrete` covers
    /// exactly its recorded args, `Parameterized` covers every
    /// instantiation whose args discharge the record's conditional
    /// bounds. Typecheck uses this for bound enforcement and
    /// `spawn`. IR's bounded dispatch never reaches this path (it
    /// goes straight to `[target, method_name]`).
    pub fn lookup_conformance(
        &self,
        target_id: GlobalRegistryId,
        protocol_id: GlobalRegistryId,
        target_args: &[ResolvedType],
    ) -> Option<&Conformance> {
        self.lookup_conformance_with(target_id, protocol_id, target_args, None, None)
    }

    /// The protocol arguments of the conformance that covers one
    /// target instantiation, with the target's type arguments substituted.
    pub fn conformance_args(
        &self,
        target_id: GlobalRegistryId,
        protocol_id: GlobalRegistryId,
        target_args: &[ResolvedType],
    ) -> Option<Vec<ResolvedType>> {
        let conformance = self.lookup_conformance(target_id, protocol_id, target_args)?;
        let substitution = crate::pipeline::unify::Substitution::from_args(target_id, target_args);
        Some(
            conformance
                .protocol_args
                .iter()
                .map(|arg| crate::pipeline::unify::substitute(arg, &substitution))
                .collect(),
        )
    }

    /// Like [`Self::lookup_conformance`], with an impl-local
    /// [`BoundOverlay`] so obligations raised inside a conditional
    /// impl body can discharge through the impl's own condition.
    pub fn lookup_conformance_with(
        &self,
        target_id: GlobalRegistryId,
        protocol_id: GlobalRegistryId,
        target_args: &[ResolvedType],
        overlay: Option<&BoundOverlay>,
        expected_protocol_args: Option<&[ResolvedType]>,
    ) -> Option<&Conformance> {
        self.conformance_records(target_id, protocol_id)?
            .iter()
            .find(|record| {
                let scope_matches = match &record.scope {
                    ConformanceScope::Concrete(args) => {
                        self.type_args_equivalent(args, target_args)
                    }
                    ConformanceScope::Parameterized { bounds } => {
                        self.conformance_bounds_satisfied(target_id, bounds, target_args, overlay)
                    }
                };
                scope_matches
                    && expected_protocol_args.is_none_or(|expected| {
                        let substitution =
                            crate::pipeline::unify::Substitution::from_args(target_id, target_args);
                        let actual = record
                            .protocol_args
                            .iter()
                            .map(|arg| crate::pipeline::unify::substitute(arg, &substitution))
                            .collect::<Vec<_>>();
                        self.type_args_equivalent(&actual, expected)
                    })
            })
    }

    /// Whether every target arg discharges its slot's conditional
    /// bounds. Unconditional records carry empty `bounds`, and a
    /// lookup with no args (head-level consumers) has nothing to
    /// check, so both zip to vacuous truth.
    fn conformance_bounds_satisfied(
        &self,
        target_id: GlobalRegistryId,
        bounds: &[Vec<ResolvedProtocolBound>],
        target_args: &[ResolvedType],
        overlay: Option<&BoundOverlay>,
    ) -> bool {
        let substitution = crate::pipeline::unify::Substitution::from_args(target_id, target_args);
        bounds.iter().zip(target_args).all(|(slot_bounds, arg)| {
            slot_bounds.iter().all(|bound| {
                let instantiated = ResolvedProtocolBound {
                    args: bound
                        .args
                        .iter()
                        .map(|arg| crate::pipeline::unify::substitute(arg, &substitution))
                        .collect(),
                    protocol_id: bound.protocol_id,
                };
                self.bound_satisfied(arg, &instantiated, overlay)
            })
        })
    }

    /// Whether `ty` discharges a `ty: protocol` obligation. Named
    /// types consult their conformance records recursively (so
    /// `List<List<Int>>: Equality` walks down). Type params
    /// discharge through universal protocols, their declared
    /// bounds, or the overlay. Tuples are structurally `Debug`,
    /// and structurally `Equality` when every element is. Other
    /// shapes (functions, unresolved) satisfy nothing.
    pub fn bound_satisfied(
        &self,
        ty: &ResolvedType,
        bound: &ResolvedProtocolBound,
        overlay: Option<&BoundOverlay>,
    ) -> bool {
        match ty {
            ResolvedType::Named {
                resolution: Resolution::Global(target_id),
                type_args,
            } => {
                if let Some(expansion) = self.alias_expansion(*target_id) {
                    return self.bound_satisfied(&expansion, bound, overlay);
                }
                self.lookup_conformance_with(
                    *target_id,
                    bound.protocol_id,
                    type_args,
                    overlay,
                    Some(&bound.args),
                )
                .is_some()
            }
            ResolvedType::Named {
                resolution: Resolution::TypeParam { owner, index },
                ..
            } => self.type_param_bound_granted(*owner, *index, bound, overlay),
            ResolvedType::Anonymous(_) | ResolvedType::Union(_) => {
                self.structural_bound_satisfied(ty, bound, overlay)
            }
            _ => false,
        }
    }

    /// A type param discharges a bound through a universal
    /// protocol, its declared bounds, or an overlay slot.
    fn type_param_bound_granted(
        &self,
        owner: GlobalRegistryId,
        index: TypeParamIndex,
        bound: &ResolvedProtocolBound,
        overlay: Option<&BoundOverlay>,
    ) -> bool {
        if bound.args.is_empty() && self.is_universal_protocol(bound.protocol_id) {
            return true;
        }
        let slot = index.as_u32() as usize;
        let declared = self
            .type_param_bounds(owner)
            .and_then(|all| all.get(slot))
            .is_some_and(|bounds| self.bounds_grant(bounds, bound));
        declared
            || overlay.is_some_and(|o| {
                o.owner == owner
                    && o.bounds
                        .get(slot)
                        .is_some_and(|bounds| self.bounds_grant(bounds, bound))
            })
    }

    /// Whether one slot's bound list contains `bound`, by protocol id
    /// and equivalent protocol args.
    fn bounds_grant(
        &self,
        bounds: &[ResolvedProtocolBound],
        bound: &ResolvedProtocolBound,
    ) -> bool {
        bounds.iter().any(|candidate| {
            candidate.protocol_id == bound.protocol_id
                && self.type_args_equivalent(&candidate.args, &bound.args)
        })
    }

    /// Conformance for the shapes with no impl target, which are
    /// tuples, function types, and anonymous unions. All three are
    /// `Debug` (functions render as `"..."`). Tuples and unions are
    /// `Equality` when every element or member is, functions by site
    /// plus captures. Unions are also `Hash` when every member is.
    /// Functions never hash because the type cannot see its captures.
    fn structural_bound_satisfied(
        &self,
        ty: &ResolvedType,
        bound: &ResolvedProtocolBound,
        overlay: Option<&BoundOverlay>,
    ) -> bool {
        if !bound.args.is_empty() {
            return false;
        }
        let Some(entry) = self.get(bound.protocol_id) else {
            return false;
        };
        if entry.identifier.package() != "Global" || entry.identifier.path().len() != 1 {
            return false;
        }
        let all_satisfy = |parts: &[ResolvedType]| {
            parts
                .iter()
                .all(|part| self.bound_satisfied(part, bound, overlay))
        };
        match (entry.identifier.last(), ty) {
            ("Debug", _) => true,
            ("Equality", ResolvedType::Anonymous(AnonymousKind::Function { .. })) => true,
            ("Equality", ResolvedType::Anonymous(AnonymousKind::Tuple { elements })) => {
                all_satisfy(elements)
            }
            ("Equality" | "Hash", ResolvedType::Union(members)) => all_satisfy(members),
            _ => false,
        }
    }

    /// Whether `protocol_id` names a universal protocol
    /// ([`UNIVERSAL_PROTOCOLS`]), which every type param satisfies
    /// without a declared bound.
    pub fn is_universal_protocol(&self, protocol_id: GlobalRegistryId) -> bool {
        self.get(protocol_id).is_some_and(|entry| {
            entry.identifier.package() == "Global"
                && entry.identifier.path().len() == 1
                && UNIVERSAL_PROTOCOLS.contains(&entry.identifier.last())
        })
    }

    /// Resolve [`UNIVERSAL_PROTOCOLS`] to their `GlobalRegistryId`s.
    /// A name that is not registered yet (e.g. before `Global.debug`
    /// has been collected) is silently skipped. Callers should only
    /// observe a non-empty list once the stdlib has loaded. Order
    /// follows the source-order of [`UNIVERSAL_PROTOCOLS`].
    pub fn universal_protocol_ids(&self) -> Vec<GlobalRegistryId> {
        UNIVERSAL_PROTOCOLS
            .iter()
            .filter_map(|name| {
                let identifier = Identifier::single("Global", *name);
                self.lookup(&identifier).map(|(id, _)| id)
            })
            .collect()
    }

    /// Whether `target_id` conforms to `protocol_id` under any
    /// instantiation. For consumers that only ask "which protocols"
    /// and have no instantiation at hand (monitor, carriers, the
    /// driver's Task check).
    pub fn conforms_any(&self, target_id: GlobalRegistryId, protocol_id: GlobalRegistryId) -> bool {
        self.conformance_records(target_id, protocol_id)
            .is_some_and(|records| !records.is_empty())
    }

    /// The protocol whose roster supplies `method/arity` on
    /// `target_id`, found among the protocols the target has any
    /// conformance record for. Method names are unique per type, so
    /// at most one protocol can claim the pair.
    pub fn protocol_declaring_method(
        &self,
        target_id: GlobalRegistryId,
        method: &str,
        arity: usize,
    ) -> Option<GlobalRegistryId> {
        self.conformance_map(target_id)?
            .keys()
            .copied()
            .find(|protocol_id| {
                let Some(GlobalKind::Protocol(Some(definition))) =
                    self.entries.get(protocol_id).map(|entry| &entry.kind)
                else {
                    return false;
                };
                definition
                    .methods
                    .iter()
                    .any(|candidate| candidate.name == method && candidate.arity == arity)
            })
    }

    /// Every recorded conformance of `target_id` to `protocol_id`,
    /// or `None` when the entry is not a builtin/struct/enum or has
    /// no record for that protocol.
    pub fn conformance_records(
        &self,
        target_id: GlobalRegistryId,
        protocol_id: GlobalRegistryId,
    ) -> Option<&[Conformance]> {
        self.conformance_map(target_id)?
            .get(&protocol_id)
            .map(Vec::as_slice)
    }

    /// The conformance map of `target_id`, or `None` when the entry
    /// is missing or its kind carries no conformances.
    fn conformance_map(&self, target_id: GlobalRegistryId) -> Option<&ConformanceMap> {
        conformance_map_of(&self.entries.get(&target_id)?.kind)
    }

    /// Whether two records for one `(target, protocol)` pair claim
    /// an overlapping set of instantiations.
    fn conformances_overlap(&self, a: &Conformance, b: &Conformance) -> bool {
        match (&a.scope, &b.scope) {
            (ConformanceScope::Parameterized { .. }, _)
            | (_, ConformanceScope::Parameterized { .. }) => true,
            (ConformanceScope::Concrete(a_args), ConformanceScope::Concrete(b_args)) => {
                self.type_args_equivalent(a_args, b_args)
            }
        }
    }

    /// Pairwise [`types_equivalent`] over two arg lists.
    fn type_args_equivalent(&self, a: &[ResolvedType], b: &[ResolvedType]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| types_equivalent(x, y, self))
    }
}

/// The conformance map a kind carries. Builtins and stamped structs
/// and enums carry one. Every other kind, and an unstamped struct or
/// enum, carries none.
fn conformance_map_of(kind: &GlobalKind) -> Option<&ConformanceMap> {
    match kind {
        GlobalKind::Builtin(def) => Some(&def.conformances),
        GlobalKind::Struct(Some(def)) => Some(&def.conformances),
        GlobalKind::Enum(Some(def)) => Some(&def.conformances),
        _ => None,
    }
}

/// Mutable twin of [`conformance_map_of`].
fn conformance_map_mut(kind: &mut GlobalKind) -> Option<&mut ConformanceMap> {
    match kind {
        GlobalKind::Builtin(def) => Some(&mut def.conformances),
        GlobalKind::Struct(Some(def)) => Some(&mut def.conformances),
        GlobalKind::Enum(Some(def)) => Some(&mut def.conformances),
        _ => None,
    }
}
