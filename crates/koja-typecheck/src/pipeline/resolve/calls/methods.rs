//! Helpers for [`super::resolve_method_call`]: the
//! [`MethodReceiver`] receiver-classification enum, the
//! [`MethodInferenceTarget`] inference-input bundle, the receiver
//! walker [`classify_receiver`], the dual-scope inference body
//! [`infer_method_call_type_args`], and the small lookup /
//! diagnostic-shape helpers ([`function_signature`],
//! [`method_lookup_message`], [`dispatch_mismatch_message`]).

use koja_ast::ast::{Arg, Diagnostic, EnumConstructionData, Expr, ExprKind};
use koja_ast::identifier::{
    AnonymousKind, GlobalRegistryId, Resolution, ResolvedType, TypeParamIndex,
};
use koja_ast::span::Span;

use super::super::ctx::{Callee, Resolver};
use super::super::expr::resolve_expr;
use super::super::inference::{
    PhantomContext, fill_from_expected, finalize_inference, unify_pairs,
};
use super::super::types::{display_resolution, lookup_type, peel_alias};
use super::emit_conflict;
use crate::pipeline::unify::{Substitution, substitute};
use crate::pipeline::visibility::check_reference_visibility;
use crate::registry::{
    Dispatch, FunctionSignature, GlobalKind, GlobalRegistry, RegistryEntry, ResolvedParam,
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
/// concrete struct id only emerges post-monomorphization. `Tuple`
/// has no registry id at all: anonymous tuples are structural and
/// admit only the universal-protocol functions, resolved by
/// [`super::tuples::resolve_tuple_method_call`].
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
    Tuple,
}

impl MethodReceiver {
    pub(super) fn expected_dispatch(self) -> Dispatch {
        match self {
            Self::Static { .. } => Dispatch::Static,
            Self::Instance { .. } | Self::Bounded { .. } | Self::Tuple => Dispatch::Instance,
        }
    }

    /// Params the user wrote against. Instance / bounded dispatch
    /// absorbs `params[0]` (`self`) into the receiver.
    pub(super) fn explicit_params(self, params: &[ResolvedParam]) -> &[ResolvedParam] {
        match self {
            Self::Static { .. } => params,
            Self::Instance { .. } | Self::Bounded { .. } | Self::Tuple => {
                params.get(1..).unwrap_or(&[])
            }
        }
    }
}

/// Inspect the receiver and pick the dispatch path. Stamps both the
/// inner `Ident` and outer `Expr` resolutions so seal sees a fully
/// populated tree.
///
/// Static dispatch admits three receiver shapes, all collapsed to a
/// dotted path by [`static_receiver_path`]:
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
    if let Some(receiver_path) = static_receiver_path(&receiver.kind)
        && let Some((struct_id, struct_entry)) =
            lookup_type(&receiver_path, resolver.resolution_scope())
        && matches!(
            struct_entry.kind,
            GlobalKind::Enum(_) | GlobalKind::Protocol(_) | GlobalKind::Struct(_)
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
    if let ResolvedType::Union(_) = peel_alias(&receiver.resolution, resolver.registry) {
        diagnostics.push(Diagnostic::error(
            format!(
                "cannot call method on union type `{}`. \
                 Match the union first to bind a specific variant",
                display_resolution(&receiver.resolution, resolver.registry),
            ),
            receiver.span,
        ));
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
        ResolvedType::Anonymous(AnonymousKind::Tuple { .. }) => Some(MethodReceiver::Tuple),
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

/// Collapse a method-call receiver to its dotted type path when one
/// of the static-dispatch shapes matches. Returns:
///
/// - `Some(["Color"])` for bare `Ident("Color")`.
/// - `Some(["Crypto", "SHA256"])` for the parser's
///   `EnumConstruction { type_path: ["Crypto"], variant: "SHA256",
///   data: Unit }` shape, what `Crypto.SHA256.digest(...)` and
///   `HTTP.Headers.new()` parse to before disambiguation.
/// - `Some(["HTTP", "Headers"])` for an `Ident`-rooted
///   `FieldAccess` chain `FieldAccess { receiver: Ident("HTTP"),
///   field: "Headers" }`.
/// - `None` for everything else (value receivers, parenthesized
///   expressions, calls, etc.). Those flow through the
///   instance-dispatch path.
fn static_receiver_path(kind: &ExprKind) -> Option<Vec<String>> {
    match kind {
        ExprKind::EnumConstruction {
            data: EnumConstructionData::Unit,
            type_path,
            variant,
        } => {
            let mut path = type_path.clone();
            path.push(variant.clone());
            Some(path)
        }
        ExprKind::Ident { .. } | ExprKind::FieldAccess { .. } => {
            let mut path = Vec::new();
            walk_dotted_path(kind, &mut path)?;
            Some(path)
        }
        _ => None,
    }
}

fn walk_dotted_path(kind: &ExprKind, out: &mut Vec<String>) -> Option<()> {
    match kind {
        ExprKind::Ident { name, .. } => {
            out.push(name.clone());
            Some(())
        }
        ExprKind::FieldAccess { receiver, field } => {
            walk_dotted_path(&receiver.kind, out)?;
            out.push(field.clone());
            Some(())
        }
        _ => None,
    }
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
        registry,
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
    match &entry.kind {
        GlobalKind::Function(Some(sig)) => Ok(sig),
        GlobalKind::Function(None) => panic!(
            "resolve method call: function `{}` has no lifted signature: \
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
        MethodReceiver::Bounded { .. } | MethodReceiver::Tuple => {
            unreachable!("bounded / tuple receivers don't reach this path")
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
        MethodReceiver::Bounded { .. } | MethodReceiver::Tuple => {
            unreachable!("bounded / tuple receivers don't reach this path")
        }
    }
}
