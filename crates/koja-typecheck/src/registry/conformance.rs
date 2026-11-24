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
    BoundOverlay, Conformance, ConformanceScope, Dispatch, GlobalKind, GlobalRegistry,
    ResolvedProtocolBound, UNIVERSAL_PROTOCOLS,
};
use crate::pipeline::resolve::types::types_equivalent;

/// The per-protocol conformance records a target entry carries.
type ConformanceMap = BTreeMap<GlobalRegistryId, Vec<Conformance>>;

/// The obligation that an alias satisfies a protocol bound, while a
/// check of it is in progress. The alias id comes first.
type AliasGoal = (GlobalRegistryId, ResolvedProtocolBound);

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
    /// target instantiation, with the target's type arguments
    /// substituted. `overlay` lets a conditional record answer from
    /// inside the impl body that grants its condition.
    pub fn conformance_args(
        &self,
        target_id: GlobalRegistryId,
        protocol_id: GlobalRegistryId,
        target_args: &[ResolvedType],
        overlay: Option<&BoundOverlay>,
    ) -> Option<Vec<ResolvedType>> {
        let conformance =
            self.lookup_conformance_with(target_id, protocol_id, target_args, overlay, None)?;
        let substitution = crate::pipeline::unify::Substitution::from_args(target_id, target_args);
        Some(
            conformance
                .protocol_args
                .iter()
                .map(|arg| crate::pipeline::unify::substitute(arg, &substitution, self))
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
        self.lookup_conformance_in(
            target_id,
            protocol_id,
            target_args,
            overlay,
            expected_protocol_args,
            &mut Vec::new(),
        )
    }

    /// [`Self::lookup_conformance_with`] inside a check that has
    /// `goals` in progress.
    fn lookup_conformance_in(
        &self,
        target_id: GlobalRegistryId,
        protocol_id: GlobalRegistryId,
        target_args: &[ResolvedType],
        overlay: Option<&BoundOverlay>,
        expected_protocol_args: Option<&[ResolvedType]>,
        goals: &mut Vec<AliasGoal>,
    ) -> Option<&Conformance> {
        self.conformance_records(target_id, protocol_id)?
            .iter()
            .find(|record| {
                let scope_matches = match &record.scope {
                    ConformanceScope::Concrete(args) => {
                        self.type_args_equivalent(args, target_args)
                    }
                    ConformanceScope::Parameterized { bounds } => self
                        .conformance_bounds_satisfied(
                            target_id,
                            bounds,
                            target_args,
                            overlay,
                            goals,
                        ),
                };
                scope_matches
                    && expected_protocol_args.is_none_or(|expected| {
                        let substitution =
                            crate::pipeline::unify::Substitution::from_args(target_id, target_args);
                        let actual = record
                            .protocol_args
                            .iter()
                            .map(|arg| crate::pipeline::unify::substitute(arg, &substitution, self))
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
        goals: &mut Vec<AliasGoal>,
    ) -> bool {
        let substitution = crate::pipeline::unify::Substitution::from_args(target_id, target_args);
        bounds.iter().zip(target_args).all(|(slot_bounds, arg)| {
            slot_bounds.iter().all(|bound| {
                let instantiated = ResolvedProtocolBound {
                    args: bound
                        .args
                        .iter()
                        .map(|arg| crate::pipeline::unify::substitute(arg, &substitution, self))
                        .collect(),
                    protocol_id: bound.protocol_id,
                };
                self.bound_satisfied_in(arg, &instantiated, overlay, goals)
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
        self.bound_satisfied_in(ty, bound, overlay, &mut Vec::new())
    }

    /// [`Self::bound_satisfied`] inside a check that has `goals` in
    /// progress.
    fn bound_satisfied_in(
        &self,
        ty: &ResolvedType,
        bound: &ResolvedProtocolBound,
        overlay: Option<&BoundOverlay>,
        goals: &mut Vec<AliasGoal>,
    ) -> bool {
        match ty {
            ResolvedType::Named {
                resolution: Resolution::Global(target_id),
                type_args,
            } => {
                if let Some(expansion) = self.alias_expansion(*target_id) {
                    return self
                        .alias_bound_satisfied(*target_id, &expansion, bound, overlay, goals);
                }
                self.lookup_conformance_in(
                    *target_id,
                    bound.protocol_id,
                    type_args,
                    overlay,
                    Some(&bound.args),
                    goals,
                )
                .is_some()
            }
            ResolvedType::Named {
                resolution: Resolution::TypeParam { owner, index },
                ..
            } => self.type_param_bound_granted(*owner, *index, bound, overlay),
            ResolvedType::Anonymous(_) | ResolvedType::Union(_) => {
                self.structural_bound_satisfied(ty, bound, overlay, goals)
            }
            _ => false,
        }
    }

    /// Whether the expansion of the alias `alias_id` discharges
    /// `bound`. The expansion of a recursive alias names the alias,
    /// so the check reaches the same obligation again. That inner
    /// obligation holds. The outer check still fails if any other
    /// part of the expansion does not conform.
    fn alias_bound_satisfied(
        &self,
        alias_id: GlobalRegistryId,
        expansion: &ResolvedType,
        bound: &ResolvedProtocolBound,
        overlay: Option<&BoundOverlay>,
        goals: &mut Vec<AliasGoal>,
    ) -> bool {
        let goal = (alias_id, bound.clone());
        if goals.contains(&goal) {
            return true;
        }
        goals.push(goal);
        let satisfied = self.bound_satisfied_in(expansion, bound, overlay, goals);
        goals.pop();
        satisfied
    }

    /// The protocol bounds one type-param slot carries. These are the
    /// owner's declared bounds for the slot, plus the overlay's slot
    /// when the overlay targets the same owner.
    pub fn granted_bounds<'a>(
        &'a self,
        owner: GlobalRegistryId,
        index: TypeParamIndex,
        overlay: Option<&'a BoundOverlay>,
    ) -> Vec<&'a ResolvedProtocolBound> {
        let slot = index.as_u32() as usize;
        let declared = self
            .type_param_bounds(owner)
            .and_then(|all| all.get(slot))
            .map(Vec::as_slice)
            .unwrap_or_default();
        let granted = overlay
            .filter(|o| o.owner == owner)
            .and_then(|o| o.bounds.get(slot))
            .map(Vec::as_slice)
            .unwrap_or_default();
        declared.iter().chain(granted).collect()
    }

    /// A type param discharges a bound through a universal
    /// protocol or through a granted bound with the same protocol
    /// and equivalent protocol args.
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
        self.granted_bounds(owner, index, overlay)
            .into_iter()
            .any(|candidate| {
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
    ///
    /// A union also satisfies any other protocol that every member
    /// satisfies, provided the protocol dispatches on the receiver
    /// alone (see [`Self::protocol_dispatches_on_receiver`]). A call
    /// through the bound then lowers to a switch on the tag with one
    /// member call per arm.
    fn structural_bound_satisfied(
        &self,
        ty: &ResolvedType,
        bound: &ResolvedProtocolBound,
        overlay: Option<&BoundOverlay>,
        goals: &mut Vec<AliasGoal>,
    ) -> bool {
        let Some(entry) = self.get(bound.protocol_id) else {
            return false;
        };
        let mut all_satisfy = |parts: &[ResolvedType]| {
            parts
                .iter()
                .all(|part| self.bound_satisfied_in(part, bound, overlay, goals))
        };
        let global_name = (bound.args.is_empty()
            && entry.identifier.package() == "Global"
            && entry.identifier.path().len() == 1)
            .then(|| entry.identifier.last());
        match (global_name, ty) {
            (Some("Debug"), _) => true,
            (Some("Equality"), ResolvedType::Anonymous(AnonymousKind::Function { .. })) => true,
            (Some("Equality"), ResolvedType::Anonymous(AnonymousKind::Tuple { elements })) => {
                all_satisfy(elements)
            }
            (Some("Equality" | "Hash"), ResolvedType::Union(members)) => all_satisfy(members),
            (_, ResolvedType::Union(members)) => {
                self.protocol_dispatches_on_receiver(bound.protocol_id) && all_satisfy(members)
            }
            _ => false,
        }
    }

    /// Whether every function of `protocol_id` takes `self` and names
    /// `Self` in no other position. Only such a protocol can dispatch
    /// on a union value, because the tag of the receiver is the one
    /// thing that picks the member impl. A static function has no
    /// receiver to read a tag from, a `Self` parameter could carry a
    /// different member than the receiver, and a `Self` result would
    /// have a different type in each arm.
    fn protocol_dispatches_on_receiver(&self, protocol_id: GlobalRegistryId) -> bool {
        let Some(GlobalKind::Protocol(Some(definition))) =
            self.get(protocol_id).map(|entry| &entry.kind)
        else {
            return false;
        };
        definition.methods.iter().all(|method| {
            method.dispatch == Dispatch::Instance
                && !names_protocol_self(&method.return_type, protocol_id)
                && method
                    .non_self_params
                    .iter()
                    .all(|param| !names_protocol_self(&param.ty, protocol_id))
        })
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

/// Whether `ty` names the implicit `Self` of `protocol_id` at any
/// depth. Lift registers `Self` as type-param slot 0 of the protocol.
fn names_protocol_self(ty: &ResolvedType, protocol_id: GlobalRegistryId) -> bool {
    let any = |parts: &[ResolvedType]| {
        parts
            .iter()
            .any(|part| names_protocol_self(part, protocol_id))
    };
    match ty {
        ResolvedType::Named {
            resolution: Resolution::TypeParam { owner, index },
            type_args,
        } => (*owner == protocol_id && index.as_u32() == 0) || any(type_args),
        ResolvedType::Named { type_args, .. } => any(type_args),
        ResolvedType::Anonymous(AnonymousKind::Function { params, ret }) => {
            any(params) || names_protocol_self(ret, protocol_id)
        }
        ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) => any(elements),
        ResolvedType::Union(members) => any(members),
        ResolvedType::Failed | ResolvedType::Unresolved => false,
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
