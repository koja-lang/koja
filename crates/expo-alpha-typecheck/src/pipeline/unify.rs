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
//! scope); every other call site uses a single scope. The substitution
//! routes by the leaf's `(owner, index)` automatically — callers don't
//! pass an explicit owner.

use expo_ast::identifier::{
    AnonymousKind, FnParam, GlobalRegistryId, Resolution, ResolvedType, TypeParamIndex,
};

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
/// enum sites construct a single-scope substitution; method calls
/// construct a dual-scope one (receiver + method). Pattern and impl
/// sites that already know their type-args build one with [`from_args`].
///
/// [`from_args`]: Substitution::from_args
#[derive(Clone, Debug)]
pub(crate) struct Substitution {
    scopes: Vec<Scope>,
}

impl Substitution {
    /// Empty substitution — every `set` is a no-op, every `get` returns
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
    /// given arities. The first scope is `receiver`; the second is
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
    pub(crate) fn from_args(owner: GlobalRegistryId, args: &[ResolvedType]) -> Self {
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
    /// filled with a *different* value; `Ok(())` on a fresh fill or a
    /// re-fill with the same value. Out-of-scope owners and out-of-
    /// range indices are silent no-ops.
    pub(crate) fn set(
        &mut self,
        owner: GlobalRegistryId,
        index: TypeParamIndex,
        value: ResolvedType,
    ) -> Result<(), Conflict> {
        let Some(scope) = self.scope_mut(owner) else {
            return Ok(());
        };
        let Some(slot) = scope.slots.get_mut(index.as_u32() as usize) else {
            return Ok(());
        };
        match slot {
            Some(prev) if *prev != value => Err(Conflict {
                actual: value,
                owner,
                param_index: index.as_u32() as usize,
                prev: prev.clone(),
            }),
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
    /// scope — call [`owns`] first when in doubt.
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

/// Walk `template` against `actual` and populate `subst` with every
/// inferred binding. Structural disagreement (e.g. `Pair` template vs
/// `Int` actual, mismatched arities, owner-out-of-scope `TypeParam`)
/// silently skips — the caller substitutes `subst` into the template
/// and re-checks downstream, surfacing a clearer diagnostic on the
/// post-substitution shape. `Resolution::Unresolved` actuals are
/// silently accepted (upstream already diagnosed).
pub(crate) fn unify_into(
    template: &ResolvedType,
    actual: &ResolvedType,
    subst: &mut Substitution,
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
                subst.set(*owner, *index, actual.clone())
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
                unify_into(sub_template, sub_actual, subst)?;
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
                unify_into(&template_param.ty, &actual_param.ty, subst)?;
            }
            unify_into(template_ret, actual_ret, subst)
        }
        _ => Ok(()),
    }
}

/// Apply `subst` to `template`: replace every `TypeParam` leaf whose
/// owner is one of the substitution's scopes with the inferred value.
/// Phantom slots substitute to [`ResolvedType::unresolved`]. Leaves
/// owned by out-of-scope owners (e.g. an outer-fn type-param when the
/// substitution only covers an inner call) round-trip unchanged.
pub(crate) fn substitute(template: &ResolvedType, subst: &Substitution) -> ResolvedType {
    match template {
        ResolvedType::Anonymous(AnonymousKind::Function { params, ret }) => {
            ResolvedType::Anonymous(AnonymousKind::Function {
                params: params
                    .iter()
                    .map(|p| FnParam {
                        mode: p.mode,
                        ty: substitute(&p.ty, subst),
                    })
                    .collect(),
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
        ResolvedType::Unresolved => ResolvedType::Unresolved,
    }
}
