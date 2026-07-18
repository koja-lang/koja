//! Type-parameter inference for generic construction sites.
//!
//! [`Substitution`] holds one or more owner scopes (a struct/enum/fn id
//! plus its type-param slot vector). [`unify_into`] walks a template
//! against an actual value, populating slots whenever the template's
//! leaf is a [`Resolution::TypeParam`] owned by one of the scopes.
//! [`substitute`] applies a populated [`Substitution`] back to a
//! template, replacing every owned `TypeParam` leaf with its inferred
//! value (`Unresolved` for slots still unfilled).
//!
//! Method calls are the dual-scope case (receiver scope + method
//! scope). Every other call site uses a single scope. The substitution
//! routes by the leaf's `(owner, index)` automatically. Callers don't
//! pass an explicit owner.

use koja_ast::identifier::{
    AnonymousKind, GlobalRegistryId, Resolution, ResolvedType, TypeParamIndex,
};

use crate::pipeline::resolve::types::{is_primitive, types_equivalent};
use crate::registry::GlobalRegistry;

/// A type parameter unified to two distinct concrete types across two
/// construction-site values. Mapped by callers into a "type parameter
/// `T` cannot be both `A` and `B`" diagnostic.
#[derive(Debug)]
pub(crate) struct Conflict {
    pub(crate) actual: ResolvedType,
    pub(crate) owner: GlobalRegistryId,
    pub(crate) param_index: usize,
    pub(crate) prev: ResolvedType,
}

/// One owner scope inside a [`Substitution`]: the registry id of the
/// generic decl that owns the params, plus per-slot inferred types
/// (`None` = phantom).
#[derive(Clone, Debug)]
struct Scope {
    owner: GlobalRegistryId,
    slots: Vec<Option<ResolvedType>>,
}

/// Inference state for one or more owner scopes. Bare-call / struct /
/// enum sites construct a single-scope substitution. Method calls
/// construct a dual-scope one (receiver + method). Pattern and impl
/// sites that already know their type-args build one with [`from_args`].
///
/// [`from_args`]: Substitution::from_args
#[derive(Clone, Debug)]
pub struct Substitution {
    scopes: Vec<Scope>,
}

impl Substitution {
    /// Empty substitution: every `set` is a no-op, every `get` returns
    /// `None`. Used when a callee has no type params at all.
    pub(crate) fn empty() -> Self {
        Self { scopes: Vec::new() }
    }

    /// Build a single-scope substitution with `arity` empty slots.
    pub(crate) fn single(owner: GlobalRegistryId, arity: usize) -> Self {
        Self {
            scopes: vec![Scope {
                owner,
                slots: vec![None; arity],
            }],
        }
    }

    /// Build a dual-scope substitution (receiver + method) with the
    /// given arities. The first scope is `receiver`, the second is
    /// `method`. Order matters for [`into_args`]/[`args`] callers that
    /// extract by position.
    pub(crate) fn dual(
        receiver: GlobalRegistryId,
        receiver_arity: usize,
        method: GlobalRegistryId,
        method_arity: usize,
    ) -> Self {
        Self {
            scopes: vec![
                Scope {
                    owner: receiver,
                    slots: vec![None; receiver_arity],
                },
                Scope {
                    owner: method,
                    slots: vec![None; method_arity],
                },
            ],
        }
    }

    /// Build a substitution pre-seeded with a scope's known type-args.
    /// Used by pattern / trait-impl conformance code that has the
    /// receiver's concrete `type_args` in hand and just wants to
    /// substitute them into a declared template.
    pub fn from_args(owner: GlobalRegistryId, args: &[ResolvedType]) -> Self {
        Self {
            scopes: vec![Scope {
                owner,
                slots: args.iter().cloned().map(Some).collect(),
            }],
        }
    }

    /// Lookup a slot. Returns `None` if `owner` isn't in scope or the
    /// slot is unfilled.
    pub(crate) fn get(
        &self,
        owner: GlobalRegistryId,
        index: TypeParamIndex,
    ) -> Option<&ResolvedType> {
        let scope = self.scope(owner)?;
        scope.slots.get(index.as_u32() as usize)?.as_ref()
    }

    /// Set a slot. Returns `Err(Conflict)` if the slot was already
    /// filled with a value that isn't [`types_equivalent`] to `value`
    /// AND isn't a union containing `value` as a member. `Ok(())` on a
    /// fresh fill or a compatible re-fill. Out-of-scope owners and
    /// out-of-range indices are silent no-ops.
    ///
    /// The compatibility check (rather than strict `prev == value`)
    /// matters most for the `fill_from_expected` path: a payload-
    /// driven bind of `T -> Int64` followed by an expected-type fill
    /// of `T -> Int` must not roll back the entire substitution and
    /// strand sibling slots (`E` etc.) unbound, since `Int` and
    /// `Int64` are the same type. Today that's the alias rule.
    /// When `Int` becomes a union over its sized variants the same
    /// predicate generalizes: `T -> Int64` then `T -> Int` still
    /// resolves cleanly because `Int64` is a member of the `Int`
    /// union.
    ///
    /// The union-member arm covers the dual case for user-declared
    /// unions: a receiver pre-bind of `M -> MsgA | MsgB` (from a
    /// `Ref<MsgA | MsgB, _>.call(...)` site) followed by an
    /// arg-driven bind of `M -> MsgA` keeps the wider slot intact
    /// rather than rejecting the call as a "cannot be both" conflict.
    /// One-direction-only: if a narrower slot value would be widened
    /// by a later union arrival, that's a `fill_from_expected` story,
    /// not this one. Leave it for the (rarer) flow-inference case.
    pub(crate) fn set(
        &mut self,
        owner: GlobalRegistryId,
        index: TypeParamIndex,
        value: ResolvedType,
        registry: &GlobalRegistry,
    ) -> Result<(), Conflict> {
        let Some(scope) = self.scope_mut(owner) else {
            return Ok(());
        };
        let Some(slot) = scope.slots.get_mut(index.as_u32() as usize) else {
            return Ok(());
        };
        match slot {
            Some(prev)
                if !types_equivalent(prev, &value, registry)
                    && !union_contains(prev, &value, registry)
                    && !literal_widens_into(prev, &value, registry) =>
            {
                Err(Conflict {
                    actual: value,
                    owner,
                    param_index: index.as_u32() as usize,
                    prev: prev.clone(),
                })
            }
            Some(_) => Ok(()),
            None => {
                *slot = Some(value);
                Ok(())
            }
        }
    }

    /// True when `owner` is one of this substitution's scopes.
    pub(crate) fn owns(&self, owner: GlobalRegistryId) -> bool {
        self.scope(owner).is_some()
    }

    /// Read-only view of `owner`'s slots. Panics if `owner` isn't in
    /// scope. Call [`owns`] first when in doubt.
    ///
    /// [`owns`]: Substitution::owns
    pub(crate) fn slots(&self, owner: GlobalRegistryId) -> &[Option<ResolvedType>] {
        &self.scope(owner).expect("owner not in substitution").slots
    }

    /// Materialize `owner`'s slots into a `Vec<ResolvedType>`,
    /// substituting [`ResolvedType::unresolved`] for phantom slots.
    /// Used by callers writing inferred type-args back onto the AST.
    pub(crate) fn args(&self, owner: GlobalRegistryId) -> Vec<ResolvedType> {
        let Some(scope) = self.scope(owner) else {
            return Vec::new();
        };
        scope
            .slots
            .iter()
            .map(|slot| slot.clone().unwrap_or_else(ResolvedType::unresolved))
            .collect()
    }

    fn scope(&self, owner: GlobalRegistryId) -> Option<&Scope> {
        self.scopes.iter().find(|scope| scope.owner == owner)
    }

    fn scope_mut(&mut self, owner: GlobalRegistryId) -> Option<&mut Scope> {
        self.scopes.iter_mut().find(|scope| scope.owner == owner)
    }
}

/// True when `prev` is a `ResolvedType::Union` whose members include
/// a type equivalent to `value`. Asymmetric: `Union ⊇ {value}` only.
/// The symmetric "value is a union containing prev" case would widen
/// an already-filled narrower slot and belongs to the
/// `fill_from_expected` flow, not the per-arg unification path.
///
/// Used by [`Substitution::set`] to accept a method-arg unification
/// like `Ref<MsgA | MsgB, _>.call(MsgA.Ping(...))`: the receiver
/// scope pre-binds `M -> MsgA | MsgB`, and the arg drives a unify of
/// `M -> MsgA`. Without this rule the per-slot compatibility check
/// would surface a spurious "cannot be both" diagnostic.
/// True when `prev` is a sized numeric primitive (`Int8`/`Int32`/...,
/// `UInt8`/..., `Float32`) and `value` is the default literal type for
/// that family (`Int` or `Float`). Lets the per-arg unifier accept a
/// re-bind driven by an integer/float literal whose AST type is the
/// default `Int`/`Float` against a slot already pinned (typically by
/// receiver seeding) to a sized variant.
///
/// The post-substitute [`super::resolve::calls::validate_arg_signature`]
/// then runs [`super::resolve::coercion::check_compatible`] against the
/// substituted param type, which:
///   - accepts literal args that fit the sized slot ([`Compatible::Coerced`])
///   - rejects non-literal `Int`/`Float` values with a clean
///     "argument expects `Int32`, got `Int`" diagnostic
///
/// The pre-existing "cannot be both X and Y" diagnostic was wrong for
/// these sites: the slot's type is authoritative once seeded, not
/// "in conflict with" a literal's default type.
fn literal_widens_into(
    prev: &ResolvedType,
    value: &ResolvedType,
    registry: &GlobalRegistry,
) -> bool {
    let int_widens = is_primitive(value, registry, "Int") && is_sized_int(prev, registry);
    let float_widens = is_primitive(value, registry, "Float") && is_sized_float(prev, registry);
    int_widens || float_widens
}

fn is_sized_int(ty: &ResolvedType, registry: &GlobalRegistry) -> bool {
    [
        "Int8", "Int16", "Int32", "Int64", "UInt8", "UInt16", "UInt32", "UInt64",
    ]
    .iter()
    .any(|name| is_primitive(ty, registry, name))
}

fn is_sized_float(ty: &ResolvedType, registry: &GlobalRegistry) -> bool {
    ["Float32", "Float64"]
        .iter()
        .any(|name| is_primitive(ty, registry, name))
}

fn union_contains(prev: &ResolvedType, value: &ResolvedType, registry: &GlobalRegistry) -> bool {
    let ResolvedType::Union(members) = prev else {
        return false;
    };
    members
        .iter()
        .any(|member| types_equivalent(member, value, registry))
}

/// Walk `template` against `actual` and populate `subst` with every
/// inferred binding. Structural disagreement (e.g. `Pair` template vs
/// `Int` actual, mismatched arities, owner-out-of-scope `TypeParam`)
/// silently skips. The caller substitutes `subst` into the template
/// and re-checks downstream, surfacing a clearer diagnostic on the
/// post-substitution shape. `Resolution::Unresolved` actuals are
/// silently accepted (upstream already diagnosed).
pub(crate) fn unify_into(
    template: &ResolvedType,
    actual: &ResolvedType,
    subst: &mut Substitution,
    registry: &GlobalRegistry,
) -> Result<(), Conflict> {
    if matches!(actual, ResolvedType::Unresolved) {
        return Ok(());
    }
    match (template, actual) {
        (
            ResolvedType::Named {
                resolution: Resolution::TypeParam { owner, index },
                ..
            },
            _,
        ) => {
            if subst.owns(*owner) {
                subst.set(*owner, *index, actual.clone(), registry)
            } else {
                Ok(())
            }
        }
        (
            ResolvedType::Named {
                resolution: template_head,
                type_args: template_args,
            },
            ResolvedType::Named {
                resolution: actual_head,
                type_args: actual_args,
            },
        ) => {
            if template_head != actual_head || template_args.len() != actual_args.len() {
                return Ok(());
            }
            for (sub_template, sub_actual) in template_args.iter().zip(actual_args) {
                unify_into(sub_template, sub_actual, subst, registry)?;
            }
            Ok(())
        }
        (
            ResolvedType::Anonymous(AnonymousKind::Function {
                params: template_params,
                ret: template_ret,
            }),
            ResolvedType::Anonymous(AnonymousKind::Function {
                params: actual_params,
                ret: actual_ret,
            }),
        ) => {
            if template_params.len() != actual_params.len() {
                return Ok(());
            }
            for (template_param, actual_param) in template_params.iter().zip(actual_params) {
                unify_into(template_param, actual_param, subst, registry)?;
            }
            unify_into(template_ret, actual_ret, subst, registry)
        }
        _ => Ok(()),
    }
}

/// Apply `subst` to `template`: replace every `TypeParam` leaf whose
/// owner is one of the substitution's scopes with the inferred value.
/// Phantom slots substitute to [`ResolvedType::unresolved`]. Leaves
/// owned by out-of-scope owners (e.g. an outer-fn type-param when the
/// substitution only covers an inner call) round-trip unchanged.
pub fn substitute(template: &ResolvedType, subst: &Substitution) -> ResolvedType {
    match template {
        ResolvedType::Anonymous(AnonymousKind::Function { params, ret }) => {
            ResolvedType::Anonymous(AnonymousKind::Function {
                params: params.iter().map(|p| substitute(p, subst)).collect(),
                ret: Box::new(substitute(ret, subst)),
            })
        }
        ResolvedType::Named {
            resolution: Resolution::TypeParam { owner, index },
            ..
        } if subst.owns(*owner) => subst
            .get(*owner, *index)
            .cloned()
            .unwrap_or_else(ResolvedType::unresolved),
        ResolvedType::Named {
            resolution,
            type_args,
        } => ResolvedType::Named {
            resolution: *resolution,
            type_args: type_args.iter().map(|arg| substitute(arg, subst)).collect(),
        },
        ResolvedType::Union(members) => {
            ResolvedType::Union(members.iter().map(|m| substitute(m, subst)).collect())
        }
        ResolvedType::Unresolved => ResolvedType::Unresolved,
    }
}

#[cfg(test)]
mod tests {
    use std::slice::from_ref;

    use koja_ast::identifier::{
        AnonymousKind, GlobalRegistryId, Resolution, ResolvedType, TypeParamIndex,
    };

    use crate::registry::GlobalRegistry;

    use super::{Substitution, substitute, unify_into};

    fn parameter(owner: GlobalRegistryId, index: u32) -> ResolvedType {
        ResolvedType::leaf(Resolution::TypeParam {
            owner,
            index: TypeParamIndex::new(index),
        })
    }

    #[test]
    fn conflicting_rebinding_reports_both_types() {
        let registry = GlobalRegistry::with_stdlib_stubs();
        let int = registry.primitive("Int");
        let owner = GlobalRegistryId::new(100);
        let string = registry.primitive("String");
        let template = parameter(owner, 0);
        let mut substitution = Substitution::single(owner, 1);

        unify_into(&template, &int, &mut substitution, &registry).expect("first binding succeeds");
        let conflict = unify_into(&template, &string, &mut substitution, &registry)
            .expect_err("incompatible rebinding should conflict");

        assert_eq!(conflict.actual, string);
        assert_eq!(conflict.prev, int);
    }

    #[test]
    fn dual_scope_substitution_routes_by_owner() {
        let registry = GlobalRegistry::with_stdlib_stubs();
        let int = registry.primitive("Int");
        let method = GlobalRegistryId::new(200);
        let receiver = GlobalRegistryId::new(100);
        let string = registry.primitive("String");
        let template = ResolvedType::Anonymous(AnonymousKind::Function {
            params: vec![parameter(receiver, 0)],
            ret: Box::new(parameter(method, 0)),
        });
        let actual = ResolvedType::Anonymous(AnonymousKind::Function {
            params: vec![int],
            ret: Box::new(string),
        });
        let mut substitution = Substitution::dual(receiver, 1, method, 1);

        unify_into(&template, &actual, &mut substitution, &registry)
            .expect("dual-scope unification should succeed");

        assert_eq!(substitute(&template, &substitution), actual);
    }

    #[test]
    fn equivalent_rebinding_preserves_first_binding() {
        let registry = GlobalRegistry::with_stdlib_stubs();
        let int = registry.primitive("Int");
        let int64 = registry.primitive("Int64");
        let owner = GlobalRegistryId::new(100);
        let template = parameter(owner, 0);
        let mut substitution = Substitution::single(owner, 1);

        unify_into(&template, &int, &mut substitution, &registry).expect("first binding succeeds");
        unify_into(&template, &int64, &mut substitution, &registry)
            .expect("equivalent rebinding succeeds");

        assert_eq!(substitute(&template, &substitution), int);
    }

    #[test]
    fn from_args_substitutes_nested_type_parameter() {
        let owner = GlobalRegistryId::new(100);
        let replacement = ResolvedType::leaf(Resolution::Global(GlobalRegistryId::new(200)));
        let substitution = Substitution::from_args(owner, from_ref(&replacement));
        let template = ResolvedType::Union(vec![
            parameter(owner, 0),
            ResolvedType::leaf(Resolution::Global(GlobalRegistryId::new(300))),
        ]);

        let ResolvedType::Union(members) = substitute(&template, &substitution) else {
            panic!("union shape should be preserved");
        };
        assert_eq!(members[0], replacement);
    }

    #[test]
    fn structural_mismatch_leaves_slot_unbound() {
        let registry = GlobalRegistry::with_stdlib_stubs();
        let owner = GlobalRegistryId::new(100);
        let string = registry.primitive("String");
        let ResolvedType::Named {
            resolution: int_head,
            ..
        } = registry.primitive("Int")
        else {
            panic!("Int should be a named primitive");
        };
        let template = ResolvedType::Named {
            resolution: int_head,
            type_args: vec![parameter(owner, 0)],
        };
        let mut substitution = Substitution::single(owner, 1);

        unify_into(&template, &string, &mut substitution, &registry)
            .expect("structural mismatch is deferred");

        assert_eq!(substitution.args(owner), vec![ResolvedType::unresolved()]);
    }
}
