//! Helpers for [`super::resolve_method_call`]: the
//! [`MethodReceiver`] receiver-classification enum, the
//! [`MethodInferenceTarget`] inference-input bundle, the receiver
//! walker [`classify_receiver`], the dual-scope inference body
//! [`infer_method_call_type_args`], and the small lookup /
//! diagnostic-shape helpers ([`function_signature`],
//! [`method_lookup_message`], [`dispatch_mismatch_message`]).

use expo_ast::ast::{Arg, Diagnostic, Expr, ExprKind};
use expo_ast::identifier::{GlobalRegistryId, Resolution, ResolvedType, TypeParamIndex};
use expo_ast::span::Span;

use super::super::ctx::{Callee, Resolver};
use super::super::expr::resolve_expr;
use super::super::structs::lookup_type;
use super::super::types::verify_bounds;
use super::{diagnose_phantom_params, emit_conflict};
use crate::pipeline::unify::{substitute_resolved_type, unify_resolved_type};
use crate::registry::{
    Dispatch, FunctionSignature, GlobalKind, GlobalRegistry, RegistryEntry, ResolvedParam,
};

/// Inputs to [`infer_method_call_type_args`] — bundles the two
/// [`Callee`]s in play (the method and its enclosing type), the
/// receiver's full resolved type (instance dispatch carries the
/// real value; static dispatch supplies an `Unresolved` placeholder
/// that the inference branch ignores), and the explicit param slice
/// (sig.params minus `self` for instance dispatch). The
/// substituted-param return still walks the full `sig.params`.
///
/// Trait-impl free type-params (e.g. `T` in `impl Show for List<T>`)
/// alias the receiver's slots, so a single `receiver_subst` covers
/// both inline struct methods and trait-impl methods.
pub(super) struct MethodInferenceTarget<'a> {
    pub(super) receiver: Callee<'a>,
    pub(super) method: Callee<'a>,
    pub(super) receiver_type: &'a ResolvedType,
    pub(super) explicit_params: &'a [ResolvedParam],
    /// Optional expected return type from the surrounding context.
    /// When provided, the inference walk unifies the method's
    /// substituted return type against it so call sites like
    /// `result: List<T> = List.new()` can constrain the receiver's
    /// `T` from the binding's annotation without ever touching args.
    pub(super) expected: Option<&'a ResolvedType>,
}

/// Receiver classification for method-call dispatch. `Static` and
/// `Instance` capture the receiver's struct id; `Bounded` captures
/// the type-param's `(owner, index)` for bounded dispatch — the
/// concrete struct id only emerges post-monomorphization.
#[derive(Clone, Copy)]
pub(super) enum MethodReceiver {
    Static {
        struct_id: GlobalRegistryId,
    },
    Instance {
        struct_id: GlobalRegistryId,
    },
    Bounded {
        owner: GlobalRegistryId,
        index: TypeParamIndex,
    },
}

impl MethodReceiver {
    pub(super) fn expected_dispatch(self) -> Dispatch {
        match self {
            Self::Static { .. } => Dispatch::Static,
            Self::Instance { .. } | Self::Bounded { .. } => Dispatch::Instance,
        }
    }

    /// Params the user wrote against. Instance / bounded dispatch
    /// absorbs `params[0]` (`self`) into the receiver.
    pub(super) fn explicit_params(self, params: &[ResolvedParam]) -> &[ResolvedParam] {
        match self {
            Self::Static { .. } => params,
            Self::Instance { .. } | Self::Bounded { .. } => params.get(1..).unwrap_or(&[]),
        }
    }
}

/// Inspect the receiver and pick the dispatch path. Stamps both the
/// inner `Ident` and outer `Expr` resolutions so seal sees a fully
/// populated tree.
pub(super) fn classify_receiver(
    receiver: &mut Expr,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<MethodReceiver> {
    if let Some(receiver_name) = bare_ident_name(&receiver.kind) {
        let receiver_path = [receiver_name.to_string()];
        if let Some((struct_id, struct_entry)) =
            lookup_type(&receiver_path, resolver.package, resolver.registry)
            && matches!(
                struct_entry.kind,
                GlobalKind::Enum(_) | GlobalKind::Struct(_)
            )
        {
            if let ExprKind::Ident {
                resolution: receiver_resolution,
                ..
            } = &mut receiver.kind
            {
                *receiver_resolution = Resolution::Global(struct_id);
            }
            receiver.resolution = ResolvedType::leaf(Resolution::Global(struct_id));
            return Some(MethodReceiver::Static { struct_id });
        }
    }

    resolve_expr(receiver, resolver, diagnostics);
    if !receiver.resolution.is_resolved() {
        // Receiver already triggered its own diagnostic.
        return None;
    }
    match &receiver.resolution {
        ResolvedType::Named {
            resolution: Resolution::Global(struct_id),
            ..
        } => {
            let struct_id = *struct_id;
            let entry = resolver.registry.get(struct_id)?;
            if !matches!(entry.kind, GlobalKind::Enum(_) | GlobalKind::Struct(_)) {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "instance method receiver must be a struct or enum value (`{}` is a {})",
                        entry.identifier,
                        entry.kind.label(),
                    ),
                    receiver.span,
                ));
                return None;
            }
            Some(MethodReceiver::Instance { struct_id })
        }
        ResolvedType::Named {
            resolution: Resolution::TypeParam { owner, index },
            ..
        } => Some(MethodReceiver::Bounded {
            owner: *owner,
            index: *index,
        }),
        _ => {
            diagnostics.push(Diagnostic::error(
                "instance method receiver must have a struct or enum type".to_string(),
                receiver.span,
            ));
            None
        }
    }
}

fn bare_ident_name(kind: &ExprKind) -> Option<&str> {
    match kind {
        ExprKind::Ident { name, .. } => Some(name.as_str()),
        _ => None,
    }
}

/// Method-call inference. Splits the substitution into two owners:
/// the method's own type-param scope and the receiver's. The receiver
/// scope is seeded by the receiver value's resolved `type_args` (for
/// instance dispatch); the method scope is populated from the
/// arg/param walk just like [`super::infer_call_type_args`].
/// `out_type_args` receives the method-scope substitution (the
/// receiver scope is already on the receiver's [`ResolvedType`] and
/// surfaces through the IR's existing struct/enum mangling).
/// Trait-impl free type-params alias the receiver's slots, so a
/// single `receiver_subst` is enough — there's no separate impl
/// scope.
/// Outputs of [`infer_method_call_type_args`] that the caller writes
/// back onto the AST + receiver shape: the method's own substituted
/// type-args (the IR's per-method monomorphization key) and the
/// receiver's substituted type-args (so static-dispatch receivers
/// can be stitched into a fully-typed [`ResolvedType::Named`]).
pub(super) struct MethodInferenceOutputs<'a> {
    pub(super) method_type_args: &'a mut Vec<ResolvedType>,
    pub(super) receiver_type_args: &'a mut Vec<ResolvedType>,
}

pub(super) fn infer_method_call_type_args(
    target: MethodInferenceTarget<'_>,
    sig: &FunctionSignature,
    args: &[Arg],
    outputs: MethodInferenceOutputs<'_>,
    call_span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> (Vec<ResolvedParam>, ResolvedType) {
    let MethodInferenceTarget {
        receiver,
        method,
        receiver_type,
        explicit_params,
        expected,
    } = target;

    let mut receiver_subst: Vec<Option<ResolvedType>> = vec![None; receiver.type_params.len()];
    let receiver_args: &[ResolvedType] = match receiver_type {
        ResolvedType::Named { type_args, .. } => type_args,
        _ => &[],
    };
    for (slot, arg) in receiver_subst.iter_mut().zip(receiver_args.iter()) {
        if arg.is_resolved() {
            *slot = Some(arg.clone());
        }
    }
    let mut method_subst: Vec<Option<ResolvedType>> = vec![None; method.type_params.len()];
    for (param, arg) in explicit_params.iter().zip(args.iter()) {
        if !arg.value.resolution.is_resolved() {
            continue;
        }
        if !method.type_params.is_empty()
            && let Err(conflict) = unify_resolved_type(
                &param.ty,
                &arg.value.resolution,
                method.id,
                &mut method_subst,
            )
        {
            emit_conflict(&method, conflict, arg.span, registry, diagnostics);
        }
        if !receiver.type_params.is_empty()
            && let Err(conflict) = unify_resolved_type(
                &param.ty,
                &arg.value.resolution,
                receiver.id,
                &mut receiver_subst,
            )
        {
            emit_conflict(&receiver, conflict, arg.span, registry, diagnostics);
        }
    }
    // Expected return type unifies as a *hint* — fills only `None`
    // slots so it can't conflict with already-inferred receiver /
    // arg types (those bindings remain authoritative). Lets shapes
    // like `result: List<T> = List.new()` flow the binding's `T`
    // into the receiver scope without disturbing fully-determined
    // contexts like `p.first()` against `Pair<Int, String>`.
    if let Some(expected) = expected
        && expected.is_resolved()
    {
        fill_remaining_from_expected(&sig.return_type, expected, method.id, &mut method_subst);
        fill_remaining_from_expected(&sig.return_type, expected, receiver.id, &mut receiver_subst);
    }
    diagnose_phantom_params(&method, &method_subst, call_span, diagnostics);
    diagnose_phantom_params(&receiver, &receiver_subst, call_span, diagnostics);
    verify_bounds(method, &method_subst, call_span, registry, diagnostics);
    verify_bounds(receiver, &receiver_subst, call_span, registry, diagnostics);
    let substituted_params: Vec<ResolvedParam> = sig
        .params
        .iter()
        .map(|p| {
            let with_method = substitute_resolved_type(&p.ty, &method_subst, method.id);
            let with_receiver =
                substitute_resolved_type(&with_method, &receiver_subst, receiver.id);
            ResolvedParam {
                mode: p.mode,
                name: p.name.clone(),
                ty: with_receiver,
            }
        })
        .collect();
    let with_method_return = substitute_resolved_type(&sig.return_type, &method_subst, method.id);
    let substituted_return =
        substitute_resolved_type(&with_method_return, &receiver_subst, receiver.id);
    *outputs.method_type_args = method_subst
        .into_iter()
        .map(|slot| slot.unwrap_or_else(ResolvedType::unresolved))
        .collect();
    *outputs.receiver_type_args = receiver_subst
        .into_iter()
        .map(|slot| slot.unwrap_or_else(ResolvedType::unresolved))
        .collect();
    (substituted_params, substituted_return)
}

/// Try to fill `None` slots of `subst` (owned by `owner`) by unifying
/// `template` (the callee's declared shape) against `actual` (the
/// expected type from the surrounding context). On conflict, walks
/// away — the expected type is only a hint; authoritative bindings
/// from args / receiver are never overridden.
fn fill_remaining_from_expected(
    template: &ResolvedType,
    actual: &ResolvedType,
    owner: GlobalRegistryId,
    subst: &mut [Option<ResolvedType>],
) {
    let mut scratch = subst.to_vec();
    if unify_resolved_type(template, actual, owner, &mut scratch).is_err() {
        return;
    }
    for (slot, filled) in subst.iter_mut().zip(scratch.into_iter()) {
        if slot.is_none() {
            *slot = filled;
        }
    }
}

pub(super) fn function_signature(entry: &RegistryEntry) -> Result<&FunctionSignature, Diagnostic> {
    match &entry.kind {
        GlobalKind::Function(Some(sig)) => Ok(sig),
        GlobalKind::Function(None) => panic!(
            "resolve method call: function `{}` has no lifted signature — \
             lift_signatures must run before resolve",
            entry.identifier,
        ),
        other => Err(Diagnostic::error(
            format!(
                "cannot call `{}`: it is a {}, not a function",
                entry.identifier,
                other.label(),
            ),
            entry.span,
        )),
    }
}

pub(super) fn method_lookup_message(
    receiver: MethodReceiver,
    struct_entry: &RegistryEntry,
    method: &str,
) -> String {
    match receiver {
        MethodReceiver::Static { .. } => format!(
            "`{}` has no static method `{method}`",
            struct_entry.identifier,
        ),
        MethodReceiver::Instance { .. } => {
            format!("`{}` has no method `{method}`", struct_entry.identifier,)
        }
        MethodReceiver::Bounded { .. } => unreachable!("bounded receivers don't reach this path"),
    }
}

pub(super) fn dispatch_mismatch_message(
    receiver: MethodReceiver,
    struct_entry: &RegistryEntry,
    method_entry: &RegistryEntry,
    method: &str,
) -> String {
    match receiver {
        MethodReceiver::Static { .. } => format!(
            "cannot call instance method `{}` as a static method — call it on a value of `{}` \
             instead",
            method_entry.identifier, struct_entry.identifier,
        ),
        MethodReceiver::Instance { .. } => format!(
            "cannot call static method `{}` on a value — call it as `{}.{method}(...)` \
             instead",
            method_entry.identifier, struct_entry.identifier,
        ),
        MethodReceiver::Bounded { .. } => unreachable!("bounded receivers don't reach this path"),
    }
}
