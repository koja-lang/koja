//! Bare-call (`f(args)`) and method-call (`recv.m(args)`) resolution.
//! Both stamp the callee's `GlobalRegistryId` on the AST and validate
//! arity + per-position types. Method calls classify the receiver into
//! a [`MethodReceiver`] (`Static` for `Type.m(...)`, `Instance` for
//! `value.m(...)`) and slice the dispatch / params accordingly.

use expo_ast::ast::{Arg, Diagnostic, Expr, ExprKind};
use expo_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};
use expo_ast::labels::expr_kind_label;
use expo_ast::span::Span;

use crate::pipeline::unify::{Conflict, substitute_resolved_type, unify_resolved_type};
use crate::registry::{
    Dispatch, FunctionSignature, GlobalKind, GlobalRegistry, RegistryEntry, ResolvedParam,
};

use super::ctx::{Callee, Resolver};
use super::expr::resolve_expr;
use super::structs::lookup_type;
use super::types::display_resolution;

/// Inputs to [`infer_method_call_type_args`] — bundles the two
/// [`Callee`]s in play (the method and its enclosing type), the
/// receiver-scope seed (instance dispatch supplies the receiver's
/// `resolution.type_args`; static dispatch supplies an empty
/// slice), and the explicit param slice (sig.params minus `self`
/// for instance dispatch). The substituted-param return still
/// walks the full `sig.params`.
struct MethodInferenceTarget<'a> {
    receiver: Callee<'a>,
    method: Callee<'a>,
    receiver_seed: &'a [ResolvedType],
    explicit_params: &'a [ResolvedParam],
}

/// Receiver classification for method-call dispatch. Captures only
/// the `struct_id` so the dispatcher can re-look-up the
/// [`RegistryEntry`] without holding a borrow across mutations.
/// Extends to enum variants by adding new variants here.
#[derive(Clone, Copy)]
enum MethodReceiver {
    Static { struct_id: GlobalRegistryId },
    Instance { struct_id: GlobalRegistryId },
}

impl MethodReceiver {
    fn struct_id(self) -> GlobalRegistryId {
        match self {
            Self::Static { struct_id } | Self::Instance { struct_id } => struct_id,
        }
    }

    fn expected_dispatch(self) -> Dispatch {
        match self {
            Self::Static { .. } => Dispatch::Static,
            Self::Instance { .. } => Dispatch::Instance,
        }
    }

    /// Params the user wrote against. Instance dispatch absorbs
    /// `params[0]` (`self`) into the receiver.
    fn explicit_params(self, params: &[ResolvedParam]) -> &[ResolvedParam] {
        match self {
            Self::Static { .. } => params,
            Self::Instance { .. } => params.get(1..).unwrap_or(&[]),
        }
    }
}

pub(super) fn resolve_call(
    callee: &mut Expr,
    args: &mut [Arg],
    type_args: &mut Vec<ResolvedType>,
    call_span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_args(args, resolver, diagnostics);

    let ExprKind::Ident {
        name,
        resolution: ident_resolution,
    } = &mut callee.kind
    else {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck only supports bare-identifier callees (got `{}`)",
                expr_kind_label(&callee.kind),
            ),
            callee.span,
        ));
        return ResolvedType::unresolved();
    };

    let candidate = Identifier::new(resolver.package, vec![name.clone()]);
    let Some((id, entry)) = resolver.registry.lookup(&candidate) else {
        diagnostics.push(Diagnostic::error(
            format!("unknown function `{name}`"),
            callee.span,
        ));
        return ResolvedType::unresolved();
    };

    let sig = match &entry.kind {
        GlobalKind::Function(Some(sig)) => sig.clone(),
        GlobalKind::Function(None) => panic!(
            "resolve_call: function `{}` has no lifted signature — \
             lift_signatures must run before resolve",
            entry.identifier,
        ),
        other => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "cannot call `{name}`: it is a {}, not a function",
                    other.label(),
                ),
                callee.span,
            ));
            return ResolvedType::unresolved();
        }
    };
    let callee_label = entry.identifier.to_string();
    let callee_identifier = entry.identifier.clone();
    let callee_type_params = entry.type_params.clone();

    *ident_resolution = Resolution::Global(id);

    if callee_type_params.is_empty() {
        validate_arg_signature(
            args,
            &sig.params,
            &callee_identifier,
            call_span,
            resolver.registry,
            diagnostics,
        );
        sig.return_type.clone()
    } else {
        let callee = Callee {
            id,
            label: &callee_label,
            type_params: &callee_type_params,
        };
        let (substituted_params, substituted_return) = infer_call_type_args(
            callee,
            &sig,
            args,
            type_args,
            call_span,
            resolver.registry,
            diagnostics,
        );
        validate_arg_signature(
            args,
            &substituted_params,
            &callee_identifier,
            call_span,
            resolver.registry,
            diagnostics,
        );
        substituted_return
    }
}

/// Drive call-site type inference for a generic callee. Unifies each
/// declared param against its arg; surfaces conflicts and phantom
/// params; populates `type_args` on the AST and returns the
/// substituted param list + return type so [`validate_arg_signature`]
/// shows concrete types in arity / type diagnostics rather than
/// leaked `T`.
fn infer_call_type_args(
    callee: Callee<'_>,
    sig: &FunctionSignature,
    args: &[Arg],
    out_type_args: &mut Vec<ResolvedType>,
    call_span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> (Vec<ResolvedParam>, ResolvedType) {
    let mut subst: Vec<Option<ResolvedType>> = vec![None; callee.type_params.len()];
    for (param, arg) in sig.params.iter().zip(args.iter()) {
        if !arg.value.resolution.is_resolved() {
            continue;
        }
        if let Err(conflict) =
            unify_resolved_type(&param.ty, &arg.value.resolution, callee.id, &mut subst)
        {
            emit_conflict(&callee, conflict, arg.span, registry, diagnostics);
        }
    }
    diagnose_phantom_params(&callee, &subst, call_span, diagnostics);
    let substituted_params = sig
        .params
        .iter()
        .map(|p| ResolvedParam {
            name: p.name.clone(),
            ty: substitute_resolved_type(&p.ty, &subst, callee.id),
        })
        .collect();
    let substituted_return = substitute_resolved_type(&sig.return_type, &subst, callee.id);
    *out_type_args = subst
        .into_iter()
        .map(|slot| slot.unwrap_or_else(ResolvedType::unresolved))
        .collect();
    (substituted_params, substituted_return)
}

fn emit_conflict(
    callee: &Callee<'_>,
    conflict: Conflict,
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnostics.push(Diagnostic::error(
        format!(
            "type parameter `{}` of `{}` cannot be both `{}` and `{}`",
            callee.type_params[conflict.param_index],
            callee.label,
            display_resolution(&conflict.prev, registry),
            display_resolution(&conflict.actual, registry),
        ),
        span,
    ));
}

/// Surface a "cannot infer" diagnostic for every type-param slot
/// that stayed `None` after the unification walk. Shared by the
/// bare-call and method-call inference paths.
fn diagnose_phantom_params(
    callee: &Callee<'_>,
    subst: &[Option<ResolvedType>],
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (index, slot) in subst.iter().enumerate() {
        if slot.is_none() {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck cannot infer type parameter `{}` of `{}` \
                     from the supplied arguments",
                    callee.type_params[index], callee.label,
                ),
                span,
            ));
        }
    }
}

/// Resolve a method-style call: classify the receiver, look up
/// `<Type>.<method>`, check dispatch matches, then validate args.
/// `out_type_args` is populated when the method or its enclosing
/// type is generic so IR lower can spawn the right monomorphization.
pub(super) fn resolve_method_call(
    receiver: &mut Expr,
    method: &str,
    args: &mut [Arg],
    out_type_args: &mut Vec<ResolvedType>,
    call_span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_args(args, resolver, diagnostics);

    let Some(method_receiver) = classify_receiver(receiver, resolver, diagnostics) else {
        return ResolvedType::unresolved();
    };

    let struct_id = method_receiver.struct_id();
    let Some(struct_entry) = resolver.registry.get(struct_id) else {
        return ResolvedType::unresolved();
    };
    let receiver_label = struct_entry.identifier.to_string();
    let receiver_type_params = struct_entry.type_params.clone();

    let mut method_path = struct_entry.identifier.path().to_vec();
    method_path.push(method.to_string());
    let method_identifier = Identifier::new(struct_entry.identifier.package(), method_path);
    let Some((method_id, method_entry)) = resolver.registry.lookup(&method_identifier) else {
        diagnostics.push(Diagnostic::error(
            method_lookup_message(method_receiver, struct_entry, method),
            call_span,
        ));
        return ResolvedType::unresolved();
    };

    let sig = match function_signature(method_entry) {
        Ok(sig) => sig.clone(),
        Err(diagnostic) => {
            diagnostics.push(diagnostic);
            return ResolvedType::unresolved();
        }
    };

    let expected = method_receiver.expected_dispatch();
    if sig.dispatch != expected {
        diagnostics.push(Diagnostic::error(
            dispatch_mismatch_message(method_receiver, struct_entry, method_entry, method),
            call_span,
        ));
        return sig.return_type.clone();
    }
    let method_label = method_entry.identifier.to_string();
    let method_identifier = method_entry.identifier.clone();
    let method_type_params = method_entry.type_params.clone();

    if receiver_type_params.is_empty() && method_type_params.is_empty() {
        validate_arg_signature(
            args,
            method_receiver.explicit_params(&sig.params),
            &method_identifier,
            call_span,
            resolver.registry,
            diagnostics,
        );
        return sig.return_type.clone();
    }

    let receiver_seed: &[ResolvedType] = match method_receiver {
        MethodReceiver::Static { .. } => &[],
        MethodReceiver::Instance { .. } => &receiver.resolution.type_args,
    };
    let target = MethodInferenceTarget {
        receiver: Callee {
            id: struct_id,
            label: &receiver_label,
            type_params: &receiver_type_params,
        },
        method: Callee {
            id: method_id,
            label: &method_label,
            type_params: &method_type_params,
        },
        receiver_seed,
        explicit_params: method_receiver.explicit_params(&sig.params),
    };
    let (substituted_params, substituted_return) = infer_method_call_type_args(
        target,
        &sig,
        args,
        out_type_args,
        call_span,
        resolver.registry,
        diagnostics,
    );
    validate_arg_signature(
        args,
        method_receiver.explicit_params(&substituted_params),
        &method_identifier,
        call_span,
        resolver.registry,
        diagnostics,
    );
    substituted_return
}

/// Method-call inference. Splits the substitution into two owners:
/// the method's own type-param scope and the receiver's. The receiver
/// scope is seeded by the receiver value's resolved `type_args` (for
/// instance dispatch); the method scope is populated from the
/// arg/param walk just like [`infer_call_type_args`]. `out_type_args`
/// receives the method-scope substitution (the receiver scope is
/// already on the receiver's [`ResolvedType`] and surfaces through
/// the IR's existing struct/enum mangling).
fn infer_method_call_type_args(
    target: MethodInferenceTarget<'_>,
    sig: &FunctionSignature,
    args: &[Arg],
    out_type_args: &mut Vec<ResolvedType>,
    call_span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> (Vec<ResolvedParam>, ResolvedType) {
    let MethodInferenceTarget {
        receiver,
        method,
        receiver_seed,
        explicit_params,
    } = target;

    let mut receiver_subst: Vec<Option<ResolvedType>> = vec![None; receiver.type_params.len()];
    for (slot, arg) in receiver_subst.iter_mut().zip(receiver_seed.iter()) {
        if arg.resolution.is_resolved() {
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
    diagnose_phantom_params(&method, &method_subst, call_span, diagnostics);
    diagnose_phantom_params(&receiver, &receiver_subst, call_span, diagnostics);
    let substituted_params: Vec<ResolvedParam> = sig
        .params
        .iter()
        .map(|p| {
            let with_method = substitute_resolved_type(&p.ty, &method_subst, method.id);
            let with_receiver =
                substitute_resolved_type(&with_method, &receiver_subst, receiver.id);
            ResolvedParam {
                name: p.name.clone(),
                ty: with_receiver,
            }
        })
        .collect();
    let with_method_return = substitute_resolved_type(&sig.return_type, &method_subst, method.id);
    let substituted_return =
        substitute_resolved_type(&with_method_return, &receiver_subst, receiver.id);
    *out_type_args = method_subst
        .into_iter()
        .map(|slot| slot.unwrap_or_else(ResolvedType::unresolved))
        .collect();
    (substituted_params, substituted_return)
}

/// Inspect the receiver and pick the dispatch path. Stamps both the
/// inner `Ident` and outer `Expr` resolutions so seal sees a fully
/// populated tree.
fn classify_receiver(
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
    let Resolution::Global(struct_id) = receiver.resolution.resolution else {
        diagnostics.push(Diagnostic::error(
            "instance method receiver must have a struct or enum type".to_string(),
            receiver.span,
        ));
        return None;
    };
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

fn bare_ident_name(kind: &ExprKind) -> Option<&str> {
    match kind {
        ExprKind::Ident { name, .. } => Some(name.as_str()),
        _ => None,
    }
}

fn function_signature(entry: &RegistryEntry) -> Result<&FunctionSignature, Diagnostic> {
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

fn method_lookup_message(
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
    }
}

fn dispatch_mismatch_message(
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
    }
}

/// Resolve every call argument. Named args diagnose up front but
/// resolution still proceeds so seal walks a populated tree.
fn resolve_args(args: &mut [Arg], resolver: &mut Resolver<'_>, diagnostics: &mut Vec<Diagnostic>) {
    for arg in args.iter_mut() {
        if let Some(name) = arg.name.as_ref() {
            diagnostics.push(Diagnostic::error(
                format!("alpha typecheck does not yet support named arguments (got `{name}`)",),
                arg.span,
            ));
        }
        resolve_expr(&mut arg.value, resolver, diagnostics);
    }
}

/// Check arg arity + per-position type compatibility. Diagnostics
/// use the callee's fully-qualified [`Identifier`].
fn validate_arg_signature(
    args: &[Arg],
    expected_params: &[ResolvedParam],
    callee: &Identifier,
    call_span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if args.len() != expected_params.len() {
        diagnostics.push(Diagnostic::error(
            format!(
                "`{callee}` expects {} argument{}, got {}",
                expected_params.len(),
                if expected_params.len() == 1 { "" } else { "s" },
                args.len(),
            ),
            call_span,
        ));
        return;
    }

    for (arg, param) in args.iter().zip(expected_params.iter()) {
        let actual = &arg.value.resolution;
        if !actual.is_resolved() {
            continue;
        }
        if actual != &param.ty {
            diagnostics.push(Diagnostic::error(
                format!(
                    "argument `{}` to `{callee}` expects `{}`, got `{}`",
                    param.name,
                    display_resolution(&param.ty, registry),
                    display_resolution(actual, registry),
                ),
                arg.span,
            ));
        }
    }
}
