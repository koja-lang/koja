//! Helpers for [`super::resolve_method_call`]: the
//! [`MethodReceiver`] receiver-classification enum, the
//! [`MethodInferenceTarget`] inference-input bundle, the receiver
//! walker [`classify_receiver`], the dual-scope inference body
//! [`infer_method_call_type_args`], and the small lookup /
//! diagnostic-shape helpers ([`function_signature`],
//! [`method_lookup_message`], [`dispatch_mismatch_message`]).

use koja_ast::ast::{Arg, Diagnostic, Expr, ExprKind};
use koja_ast::identifier::{
    AnonymousKind, GlobalRegistryId, Resolution, ResolvedType, TypeParamIndex,
};
use koja_ast::span::Span;

use super::super::ctx::{BoundContext, Callee, Resolver};
use super::super::expr::resolve_expr;
use super::super::inference::{
    PhantomContext, fill_from_expected, finalize_inference, unify_pairs,
};
use super::super::paths::static_dotted_path;
use super::super::types::{display_resolution, lookup_type, peel_alias};
use super::emit_conflict;
use super::structural::StructuralShape;
use crate::pipeline::unify::{Substitution, substitute};
use crate::pipeline::visibility::check_reference_visibility;
use crate::registry::{
    ConformanceScope, Dispatch, FunctionSignature, GlobalKind, GlobalRegistry, RegistryEntry,
    ResolvedParam,
};

/// Inputs to [`infer_method_call_type_args`]. Bundles the two
/// [`Callee`]s in play (the method and its enclosing type), the
/// receiver's full resolved type (instance dispatch carries the
/// real value, static dispatch supplies an `Unresolved` placeholder
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
/// `Instance` capture the receiver's struct id. `Bounded` captures
/// the type-param's `(owner, index)` for bounded dispatch, since the
/// concrete struct id only emerges post-monomorphization.
/// `Structural` has no registry id at all: tuples, function types,
/// and unions admit only the universal-protocol functions, resolved
/// by [`super::structural::resolve_structural_method_call`].
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
    Structural(StructuralShape),
}

impl MethodReceiver {
    pub(super) fn expected_dispatch(self) -> Dispatch {
        match self {
            Self::Static { .. } => Dispatch::Static,
            Self::Instance { .. } | Self::Bounded { .. } | Self::Structural(_) => {
                Dispatch::Instance
            }
        }
    }

    /// Params the user wrote against. Instance / bounded dispatch
    /// absorbs `params[0]` (`self`) into the receiver.
    pub(super) fn explicit_params(self, params: &[ResolvedParam]) -> &[ResolvedParam] {
        match self {
            Self::Static { .. } => params,
            Self::Instance { .. } | Self::Bounded { .. } | Self::Structural(_) => {
                params.get(1..).unwrap_or(&[])
            }
        }
    }

    pub(super) fn explicit_params_for_arity(self, arity: usize) -> usize {
        match self {
            Self::Static { .. } => arity,
            Self::Instance { .. } | Self::Bounded { .. } | Self::Structural(_) => {
                arity.saturating_sub(1)
            }
        }
    }
}

/// Inspect the receiver and pick the dispatch path. Stamps both the
/// inner `Ident` and outer `Expr` resolutions so seal sees a fully
/// populated tree.
///
/// Static dispatch admits three receiver shapes, all collapsed to a
/// dotted path by [`static_dotted_path`]:
///
/// - Bare `Ident` naming a same-package or `Global` type
///   (`Color.foo()`).
/// - `EnumConstruction` with `Unit` data and TypeIdent segments,
///   the parser shape for `Pkg.Type.method(...)` because
///   `Pkg.Type` reads as a unit-variant construction until the
///   trailing method call disambiguates it. This is the parser
///   shape for both `Crypto.SHA256.digest(...)` and
///   `HTTP.Headers.new()`.
/// - `FieldAccess` chain over `Ident`s: covers paths whose tail
///   segment is a lowercase ident before a dotted method (rare, but
///   semantically equivalent and cheap to support alongside the
///   other shapes).
///
/// The receiver is rewritten to a synthetic `Ident { name:
/// "<joined.path>", resolution: Global(struct_id) }` so the IR
/// lowering's existing `Ident`-based static-receiver path picks it
/// up without further branching, and seal accepts the rewritten
/// node by virtue of its `Global` resolution.
pub(super) fn classify_receiver(
    receiver: &mut Expr,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<MethodReceiver> {
    // Protocols admit static dispatch only (statics registered via
    // `extend`, like `Process.monitor`). There is no instance path
    // for them, so no Instance-arm counterpart below.
    if let Some(receiver_path) = static_dotted_path(&receiver.kind)
        && let Some((struct_id, struct_entry)) =
            lookup_type(&receiver_path, resolver.resolution_scope())
        && matches!(
            struct_entry.kind,
            GlobalKind::Builtin(_)
                | GlobalKind::Enum(_)
                | GlobalKind::Protocol(_)
                | GlobalKind::Struct(_)
        )
    {
        check_reference_visibility(struct_entry, resolver.package, receiver.span, diagnostics);
        rewrite_to_static_ident(receiver, &receiver_path, struct_id);
        return Some(MethodReceiver::Static { struct_id });
    }

    resolve_expr(receiver, resolver, diagnostics);
    if !receiver.resolution.is_resolved() {
        // Receiver already triggered its own diagnostic.
        return None;
    }
    let structural_receiver = peel_alias(&receiver.resolution, resolver.registry);
    match structural_receiver {
        ResolvedType::Named {
            resolution: Resolution::Global(struct_id),
            ..
        } => {
            let entry = resolver.registry.get(struct_id)?;
            if !matches!(
                entry.kind,
                GlobalKind::Builtin(_) | GlobalKind::Enum(_) | GlobalKind::Struct(_)
            ) {
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
        ResolvedType::Anonymous(AnonymousKind::Function { .. }) => {
            Some(MethodReceiver::Structural(StructuralShape::Function))
        }
        ResolvedType::Anonymous(AnonymousKind::Tuple { .. }) => {
            Some(MethodReceiver::Structural(StructuralShape::Tuple))
        }
        ResolvedType::Union(_) => Some(MethodReceiver::Structural(StructuralShape::Union)),
        ResolvedType::Named {
            resolution: Resolution::TypeParam { owner, index },
            ..
        } => Some(MethodReceiver::Bounded { owner, index }),
        _ => {
            diagnostics.push(Diagnostic::error(
                "instance method receiver must have a struct or enum type".to_string(),
                receiver.span,
            ));
            None
        }
    }
}

/// Reject an instance call to a protocol method when the receiver's
/// instantiation does not discharge the impl's conditional bounds.
/// `List<fn () -> Int>` carries `List`'s `equals?` in the flat method
/// namespace, but only `List<T: Equality>` conforms, so the call has
/// no body to monomorphize. Returns `true` after pushing the
/// diagnostic. Concrete impls (`impl P for Bag<Int>`) are left to the
/// receiver-domain check after inference, which names the impl's
/// own instantiation.
pub(super) fn diagnose_unmet_conformance(
    receiver: &Expr,
    struct_id: GlobalRegistryId,
    method: &str,
    arity: usize,
    call_span: Span,
    resolver: &Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let registry = resolver.registry;
    let Some(protocol_id) = registry.protocol_declaring_method(struct_id, method, arity) else {
        return false;
    };
    let conditional = registry
        .conformance_records(struct_id, protocol_id)
        .is_some_and(|records| {
            records
                .iter()
                .any(|record| matches!(record.scope, ConformanceScope::Parameterized { .. }))
        });
    let structural = peel_alias(&receiver.resolution, registry);
    let ResolvedType::Named { type_args, .. } = &structural else {
        return false;
    };
    // Inference may still be filling the args (`List.new().equals?`),
    // and an unresolved slot satisfies no bound. Let the later
    // inference diagnostics own that case.
    if !conditional
        || !structural.is_resolved()
        || registry
            .lookup_conformance_with(
                struct_id,
                protocol_id,
                type_args,
                resolver.bound_overlay,
                None,
            )
            .is_some()
    {
        return false;
    }
    let protocol_label = registry
        .get(protocol_id)
        .map(|entry| entry.identifier.last().to_string())
        .unwrap_or_else(|| format!("<id {protocol_id}>"));
    diagnostics.push(Diagnostic::error(
        format!(
            "`{}` does not implement `{protocol_label}`, so `{method}` is unavailable",
            display_resolution(&receiver.resolution, registry),
        ),
        call_span,
    ));
    true
}

/// Rewrite the receiver expression in place to a synthetic
/// `Ident { name: "<joined.path>", resolution: Global(struct_id) }`
/// so the IR lowering's existing `Ident`-based static-receiver
/// path lands on a familiar shape regardless of whether the parser
/// produced an `Ident`, an `EnumConstruction`, or a `FieldAccess`
/// chain. The synthesized name is display-only. Downstream type
/// checks read the `Global(struct_id)` resolution off the inner
/// node and the leaf [`ResolvedType`] off the outer `Expr`.
fn rewrite_to_static_ident(receiver: &mut Expr, path: &[String], struct_id: GlobalRegistryId) {
    receiver.kind = ExprKind::Ident {
        name: path.join("."),
        resolution: Resolution::Global(struct_id),
    };
    receiver.resolution = ResolvedType::leaf(Resolution::Global(struct_id));
}

/// Method-call inference. Splits the substitution into two owners:
/// the method's own type-param scope and the receiver's. The receiver
/// scope is seeded by the receiver value's resolved `type_args` (for
/// instance dispatch). The method scope is populated from the
/// arg/param walk just like [`super::infer_call_type_args`].
/// `out_type_args` receives the method-scope substitution (the
/// receiver scope is already on the receiver's [`ResolvedType`] and
/// surfaces through the IR's existing struct/enum mangling).
/// Trait-impl free type-params alias the receiver's slots, so a
/// single `receiver_subst` is enough, there's no separate impl
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
    ctx: BoundContext<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> (Vec<ResolvedParam>, ResolvedType) {
    let registry = ctx.registry;
    let MethodInferenceTarget {
        receiver,
        method,
        receiver_type,
        explicit_params,
        expected,
    } = target;

    let mut subst = Substitution::dual(
        receiver.id,
        receiver.type_params.len(),
        method.id,
        method.type_params.len(),
    );
    seed_receiver_subst(&mut subst, receiver.id, receiver_type, registry);
    seed_impl_args_subst(&mut subst, receiver.id, &sig.impl_args, registry);
    // Mirror `infer_call_type_args`'s speculative pre-seed: lets
    // binding annotations pin sized-numeric type params before
    // arg-driven default-literal types lock in.
    if let Some(pre_seeded) = try_pre_seeded_method_subst(
        &subst,
        &sig.return_type,
        explicit_params,
        args,
        expected,
        registry,
    ) {
        subst = pre_seeded;
    } else {
        let pairs = explicit_params
            .iter()
            .zip(args.iter())
            .map(|(param, arg)| (&param.ty, &arg.value.resolution, arg.span));
        unify_pairs(pairs, &mut subst, registry, |conflict, arg_span| {
            let scope_callee = if conflict.owner == method.id {
                &method
            } else {
                &receiver
            };
            emit_conflict(scope_callee, conflict, arg_span, registry, diagnostics);
        });
        if let Some(hint) = expected {
            fill_from_expected(&sig.return_type, hint, &mut subst, registry);
        }
    }
    finalize_inference(
        &[method, receiver],
        &subst,
        &PhantomContext::Arguments,
        call_span,
        ctx,
        diagnostics,
    );
    let substituted_params: Vec<ResolvedParam> = sig
        .params
        .iter()
        .map(|p| ResolvedParam {
            name: p.name.clone(),
            ty: substitute(&p.ty, &subst),
        })
        .collect();
    let substituted_return = substitute(&sig.return_type, &subst);
    *outputs.method_type_args = subst.args(method.id);
    *outputs.receiver_type_args = subst.args(receiver.id);
    (substituted_params, substituted_return)
}

/// Speculative pre-seed for [`infer_method_call_type_args`]. Mirrors
/// `try_pre_seeded_subst` in the bare-call path with the
/// receiver-seeded `baseline` as the starting substitution.
fn try_pre_seeded_method_subst(
    baseline: &Substitution,
    return_type: &ResolvedType,
    explicit_params: &[ResolvedParam],
    args: &[Arg],
    expected: Option<&ResolvedType>,
    registry: &GlobalRegistry,
) -> Option<Substitution> {
    let hint = expected?;
    let mut scratch = baseline.clone();
    fill_from_expected(return_type, hint, &mut scratch, registry);
    let mut had_conflict = false;
    let pairs = explicit_params
        .iter()
        .zip(args.iter())
        .map(|(param, arg)| (&param.ty, &arg.value.resolution, arg.span));
    unify_pairs(pairs, &mut scratch, registry, |_, _| {
        had_conflict = true;
    });
    (!had_conflict).then_some(scratch)
}

/// Pre-fill the receiver scope with the receiver value's resolved
/// type-args. Lets `Pair<Int, String>.first()` pin `T = Int` from the
/// receiver alone, before any arg unification.
pub(super) fn seed_receiver_subst(
    subst: &mut Substitution,
    receiver_id: GlobalRegistryId,
    receiver_type: &ResolvedType,
    registry: &GlobalRegistry,
) {
    let ResolvedType::Named { type_args, .. } = receiver_type else {
        return;
    };
    seed_receiver_args(subst, receiver_id, type_args, registry);
}

/// Seed the receiver scope from a method's concrete `impl_args`
/// pinning. A method declared in `extend CPtr<UInt8>` only exists for
/// `T = UInt8`, so statics whose signatures never mention `T` (e.g.
/// `CPtr.borrow(bytes: Binary)`) still infer cleanly. Conflicts with
/// an already-seeded receiver slot are ignored here because the
/// extend-domain check downstream owns that diagnostic.
pub(super) fn seed_impl_args_subst(
    subst: &mut Substitution,
    receiver_id: GlobalRegistryId,
    impl_args: &[ResolvedType],
    registry: &GlobalRegistry,
) {
    seed_receiver_args(subst, receiver_id, impl_args, registry);
}

fn seed_receiver_args(
    subst: &mut Substitution,
    receiver_id: GlobalRegistryId,
    type_args: &[ResolvedType],
    registry: &GlobalRegistry,
) {
    for (index, arg) in type_args.iter().enumerate() {
        if arg.is_resolved() {
            let _ = subst.set(
                receiver_id,
                TypeParamIndex::new(index as u32),
                arg.clone(),
                registry,
            );
        }
    }
}

pub(super) fn function_signature(entry: &RegistryEntry) -> Result<&FunctionSignature, Diagnostic> {
    if entry.function_definition().is_none() {
        return Err(Diagnostic::error(
            format!(
                "cannot call `{}` because it is a {}, not a function",
                entry.identifier,
                entry.kind.label(),
            ),
            entry.span,
        ));
    }
    Ok(entry.expect_function_signature())
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
        MethodReceiver::Bounded { .. } | MethodReceiver::Structural(_) => {
            unreachable!("bounded / structural receivers don't reach this path")
        }
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
            "cannot call instance method `{}` as a static method. Call it on a value of `{}` \
             instead",
            method_entry.identifier, struct_entry.identifier,
        ),
        MethodReceiver::Instance { .. } => format!(
            "cannot call static method `{}` on a value. Call it as `{}.{method}(...)` \
             instead",
            method_entry.identifier, struct_entry.identifier,
        ),
        MethodReceiver::Bounded { .. } | MethodReceiver::Structural(_) => {
            unreachable!("bounded / structural receivers don't reach this path")
        }
    }
}
