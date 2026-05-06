//! Bare-call (`f(args)`) and method-call (`recv.m(args)`) resolution.
//!
//! Bare calls accept only `Ident` callees; the inner
//! `Ident.resolution` is stamped with the callee's
//! [`GlobalRegistryId`]; the outer callee `Expr.resolution` stays
//! `Unresolved` (seal carves this out) because function names aren't
//! first-class values yet. The call-site `Expr.resolution` takes the
//! callee's return type.
//!
//! Method calls run through a single dispatcher,
//! [`resolve_method_call`], that classifies the receiver into a
//! [`MethodReceiver`] (`Static` when the receiver is a bare `Ident`
//! naming a struct, `Instance` when the receiver is any expression
//! resolving to a struct value) and then walks a uniform path:
//! receiver-typestamp → method lookup on `<Type>.<method>` →
//! dispatch-axis check (`signature.dispatch == expected`) → arg
//! validation. The `Static` branch validates `args` against
//! `signature.params`; the `Instance` branch validates against
//! `signature.params[1..]` (the implicit receiver fills `params[0]`).
//!
//! [`GlobalRegistryId`]: expo_ast::identifier::GlobalRegistryId

use expo_ast::ast::{Arg, Diagnostic, Expr, ExprKind};
use expo_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

use crate::labels::expr_kind_label;
use crate::registry::{
    Dispatch, FunctionSignature, GlobalKind, GlobalRegistry, RegistryEntry, ResolvedParam,
};

use super::ctx::Resolver;
use super::expr::resolve_expr;
use super::structs::lookup_struct;
use super::types::display_resolution;

/// Receiver classification for method-call dispatch. Each variant
/// captures only the `struct_id` so we can re-look-up the
/// `RegistryEntry` whenever needed without dragging a borrow across
/// the dispatcher's mutable scope work.
///
/// Designed to extend cleanly to enum variants when they land
/// (`MethodReceiver::EnumVariant { enum_id, variant }`): the
/// classify → look-up → validate skeleton stays the same; only the
/// per-variant lookup path differs.
#[derive(Clone, Copy)]
enum MethodReceiver {
    /// `Type.method(args)` — receiver is a bare `Ident` naming a
    /// registered struct.
    Static { struct_id: GlobalRegistryId },
    /// `value.method(args)` — receiver is any expression whose
    /// resolved type is a struct.
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

    /// Slice the params the user wrote against. Static dispatch
    /// hands every param over; instance dispatch absorbs `params[0]`
    /// into the receiver.
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
        GlobalKind::Function(Some(sig)) => sig,
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

    *ident_resolution = Resolution::Global(id);
    validate_arg_signature(
        args,
        &sig.params,
        &entry.identifier,
        call_span,
        resolver.registry,
        diagnostics,
    );
    sig.return_type.clone()
}

/// Resolve a method-style call: classify the receiver, look up the
/// method, validate dispatch + args. See module docs for the full
/// algorithm.
pub(super) fn resolve_method_call(
    receiver: &mut Expr,
    method: &str,
    args: &mut [Arg],
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

    let mut method_path = struct_entry.identifier.path().to_vec();
    method_path.push(method.to_string());
    let method_identifier = Identifier::new(struct_entry.identifier.package(), method_path);
    let Some((_, method_entry)) = resolver.registry.lookup(&method_identifier) else {
        diagnostics.push(Diagnostic::error(
            method_lookup_message(method_receiver, struct_entry, method),
            call_span,
        ));
        return ResolvedType::unresolved();
    };

    let sig = match function_signature(method_entry) {
        Ok(sig) => sig,
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

    validate_arg_signature(
        args,
        method_receiver.explicit_params(&sig.params),
        &method_entry.identifier,
        call_span,
        resolver.registry,
        diagnostics,
    );
    sig.return_type.clone()
}

/// Inspect the receiver and pick the dispatch path. Stamps the
/// receiver's resolution as a side effect — both the inner `Ident`
/// and the outer `Expr` get the struct's id so seal sees a fully
/// populated tree.
fn classify_receiver(
    receiver: &mut Expr,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<MethodReceiver> {
    if let Some(receiver_name) = bare_ident_name(&receiver.kind) {
        let receiver_path = [receiver_name.to_string()];
        if let Some((struct_id, struct_entry)) =
            lookup_struct(&receiver_path, resolver.package, resolver.registry)
            && matches!(struct_entry.kind, GlobalKind::Struct(_))
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
            "instance method receiver must have a struct type".to_string(),
            receiver.span,
        ));
        return None;
    };
    let entry = resolver.registry.get(struct_id)?;
    if !matches!(entry.kind, GlobalKind::Struct(_)) {
        diagnostics.push(Diagnostic::error(
            format!(
                "instance method receiver must be a struct value (`{}` is a {})",
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

/// Resolve every call/method-call argument expression. Named
/// arguments diagnose up front so nested resolution still proceeds
/// (gives `seal_expr` a populated tree to walk).
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

/// Check argument arity + per-position type compatibility against a
/// list of declared params. Diagnostics use the callee's
/// fully-qualified [`Identifier`] so the user sees `TestApp.Point.at`
/// rather than just `at`.
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
            // Arg already triggered its own diagnostic; skip the
            // follow-up to avoid noise.
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
