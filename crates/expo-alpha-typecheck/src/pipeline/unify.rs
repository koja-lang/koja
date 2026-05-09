//! Type-parameter inference at construction sites. `resolve` calls
//! [`unify_resolved_type`] for each (declared field/payload type,
//! supplied value type) pair and walks both in lockstep, populating
//! `subst[index]` whenever the template's leaf is a
//! [`Resolution::TypeParam`]. Structural mismatches between template
//! and actual are silently skipped — the caller substitutes `subst`
//! into declared types and re-checks, so the user-facing diagnostic
//! shows concrete types rather than leaked `T`s.
//!
//! [`substitute_resolved_type`] applies a populated `subst` to a
//! template, replacing `TypeParam { owner, index }` leaves with
//! `subst[index]`. Slots still `None` (Phantom params) substitute
//! to [`ResolvedType::unresolved`] so downstream walks see a
//! non-leaky terminal.

use expo_ast::identifier::{AnonymousKind, FnParam, GlobalRegistryId, Resolution, ResolvedType};

/// A type parameter was inferred to two incompatible concrete types
/// across two construction-site fields/args. The caller maps this
/// into a "type parameter `T` cannot be both `A` and `B`" diagnostic.
#[derive(Debug)]
pub(crate) struct Conflict {
    pub(crate) param_index: usize,
    pub(crate) prev: ResolvedType,
    pub(crate) actual: ResolvedType,
}

/// Walk `template` (from a generic decl; may contain
/// [`Resolution::TypeParam`] leaves) against `actual` (from a
/// construction-site value) and populate `subst` with inferred
/// concrete types. The first binding wins on conflict so subsequent
/// substitution is deterministic; the [`Conflict`] is reported back
/// to the caller for diagnostics.
///
/// Structural disagreement (`Pair` template vs `Int` actual, mixed
/// arities, owner-mismatched `TypeParam`) silently skips — the
/// post-substitution equality check on the surrounding field/arg
/// surfaces a clearer diagnostic. `Resolution::Unresolved` on the
/// actual side is silently accepted (upstream already diagnosed).
pub(crate) fn unify_resolved_type(
    template: &ResolvedType,
    actual: &ResolvedType,
    owner: GlobalRegistryId,
    subst: &mut [Option<ResolvedType>],
) -> Result<(), Conflict> {
    if matches!(actual, ResolvedType::Unresolved) {
        return Ok(());
    }
    match (template, actual) {
        (
            ResolvedType::Named {
                resolution:
                    Resolution::TypeParam {
                        owner: param_owner,
                        index,
                    },
                ..
            },
            _,
        ) => {
            if *param_owner != owner {
                return Ok(());
            }
            let slot = &mut subst[index.as_u32() as usize];
            match slot {
                Some(prev) if prev != actual => Err(Conflict {
                    param_index: index.as_u32() as usize,
                    prev: prev.clone(),
                    actual: actual.clone(),
                }),
                Some(_) => Ok(()),
                None => {
                    *slot = Some(actual.clone());
                    Ok(())
                }
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
                unify_resolved_type(sub_template, sub_actual, owner, subst)?;
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
                unify_resolved_type(&template_param.ty, &actual_param.ty, owner, subst)?;
            }
            unify_resolved_type(template_ret, actual_ret, owner, subst)
        }
        _ => Ok(()),
    }
}

/// Substitute `subst` into `template`, replacing any
/// [`Resolution::TypeParam { owner, index }`][Resolution::TypeParam]
/// leaf whose `owner` matches with `subst[index]`. `None` slots
/// (Phantom params) substitute to [`ResolvedType::unresolved`].
/// `Named` heads recurse into `type_args`; `Anonymous` heads recurse
/// into params and return type.
pub(crate) fn substitute_resolved_type(
    template: &ResolvedType,
    subst: &[Option<ResolvedType>],
    owner: GlobalRegistryId,
) -> ResolvedType {
    match template {
        ResolvedType::Named {
            resolution:
                Resolution::TypeParam {
                    owner: param_owner,
                    index,
                },
            ..
        } if *param_owner == owner => subst
            .get(index.as_u32() as usize)
            .and_then(Option::as_ref)
            .cloned()
            .unwrap_or_else(ResolvedType::unresolved),
        ResolvedType::Named {
            resolution,
            type_args,
        } => ResolvedType::Named {
            resolution: *resolution,
            type_args: type_args
                .iter()
                .map(|arg| substitute_resolved_type(arg, subst, owner))
                .collect(),
        },
        ResolvedType::Anonymous(AnonymousKind::Function { params, ret }) => {
            ResolvedType::Anonymous(AnonymousKind::Function {
                params: params
                    .iter()
                    .map(|p| FnParam {
                        mode: p.mode,
                        ty: substitute_resolved_type(&p.ty, subst, owner),
                    })
                    .collect(),
                ret: Box::new(substitute_resolved_type(ret, subst, owner)),
            })
        }
        ResolvedType::Unresolved => ResolvedType::Unresolved,
    }
}
