//! Expression dispatch: pattern-matches `ExprKind` and routes to the
//! per-shape resolver in [`super::control_flow`] (if/unless),
//! [`super::ops`] (literal/binary/unary), or this module (calls,
//! groups, idents, static method calls). Every successful arm returns
//! the [`ResolvedType`] to stamp on `expr.resolution`.
//!
//! # Call resolution
//!
//! Bare `f(args)` accepts only `Ident` callees. The inner
//! `Ident.resolution` is stamped with the callee's
//! [`GlobalRegistryId`]; the outer callee `Expr.resolution` stays
//! `Unresolved` (seal carves this out) because function names aren't
//! first-class values yet. The call-site `Expr.resolution` takes the
//! callee's return type.
//!
//! `Type.method(args)` parses as a [`ExprKind::MethodCall`] whose
//! receiver is `Ident { name: "Type" }`. We dispatch on the receiver
//! shape: when `Type` resolves to a registered struct, we stamp the
//! struct id on the receiver's `Ident.resolution` and look up
//! `(package, [Type, method])` in the registry. Anything else is an
//! instance method call, which is still a feature gap.
//!
//! [`GlobalRegistryId`]: expo_ast::identifier::GlobalRegistryId

use expo_ast::ast::{Arg, Diagnostic, Expr, ExprKind, FieldInit, StringPart};
use expo_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

use crate::labels::expr_kind_label;
use crate::registry::{
    FunctionSignature, GlobalKind, GlobalRegistry, RegistryEntry, StructDefinition,
};

use super::control_flow::{resolve_if, resolve_unless};
use super::ops::{binary_type, literal_type, unary_type};
use super::types::display_resolution;

pub(super) fn resolve_expr(
    expr: &mut Expr,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let ty = match &mut expr.kind {
        ExprKind::Binary { op, left, right } => {
            resolve_expr(left, package, registry, diagnostics);
            resolve_expr(right, package, registry, diagnostics);
            binary_type(*op, left, right, expr.span, registry, diagnostics)
        }
        ExprKind::Call { callee, args } => {
            resolve_call(callee, args, expr.span, package, registry, diagnostics)
        }
        ExprKind::FieldAccess { receiver, field } => {
            resolve_field_access(receiver, field, expr.span, package, registry, diagnostics)
        }
        ExprKind::Group { expr: inner } => {
            resolve_expr(inner, package, registry, diagnostics);
            inner.resolution.clone()
        }
        ExprKind::Ident { name, .. } => {
            // Local references (including parameter uses) are not yet
            // supported. `Resolution::Local` lands with the follow-up
            // slice; until then emit a dedicated diagnostic.
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck does not yet support identifier references in function \
                     bodies (got `{name}`)",
                ),
                expr.span,
            ));
            ResolvedType::unresolved()
        }
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => resolve_if(
            condition,
            then_body,
            else_body.as_deref_mut(),
            expr.span,
            package,
            registry,
            diagnostics,
        ),
        ExprKind::Literal { value } => literal_type(value, registry),
        ExprKind::MethodCall {
            receiver,
            method,
            args,
        } => resolve_method_call(
            receiver,
            method,
            args,
            expr.span,
            package,
            registry,
            diagnostics,
        ),
        ExprKind::String { parts, .. } => resolve_string(parts, expr.span, registry, diagnostics),
        ExprKind::StructConstruction { type_path, fields } => resolve_struct_construction(
            type_path,
            fields,
            expr.span,
            package,
            registry,
            diagnostics,
        ),
        ExprKind::Unary { op, operand } => {
            resolve_expr(operand, package, registry, diagnostics);
            unary_type(*op, operand, expr.span, registry, diagnostics)
        }
        ExprKind::Unless { condition, body } => {
            resolve_unless(condition, body, package, registry, diagnostics)
        }
        // Unsupported shapes diagnose and leave the expression
        // unresolved. Seal runs only on the success path, so an
        // `Unresolved` leaf here is harmless — diagnostics is non-empty
        // and `check_program` returns early.
        other => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck does not yet support expression `{}`",
                    expr_kind_label(other)
                ),
                expr.span,
            ));
            ResolvedType::unresolved()
        }
    };
    expr.resolution = ty;
}

fn resolve_call(
    callee: &mut Expr,
    args: &mut [Arg],
    call_span: Span,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_args(args, package, registry, diagnostics);

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

    let candidate = Identifier::new(package, vec![name.clone()]);
    let Some((id, entry)) = registry.lookup(&candidate) else {
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
        sig,
        &entry.identifier,
        call_span,
        registry,
        diagnostics,
    );
    sig.return_type.clone()
}

/// Resolve a static method call `Type.method(args)`. Receiver shape
/// must be a bare `Ident` whose `name` resolves to a registered
/// struct in `package` (or in `Global` for stdlib stubs); anything
/// else surfaces the instance-method feature gap.
///
/// On the success path we stamp the receiver `Ident.resolution` with
/// the struct's id (so seal sees a non-`Unresolved` receiver) and
/// look up `(struct_pkg, [...struct_path, method])` in the registry.
/// IR lower then reads the same identifier off the receiver to
/// rebuild the method's `IRSymbol` without re-running this lookup.
fn resolve_method_call(
    receiver: &mut Expr,
    method: &str,
    args: &mut [Arg],
    call_span: Span,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_args(args, package, registry, diagnostics);

    let ExprKind::Ident {
        name: receiver_name,
        resolution: receiver_resolution,
    } = &mut receiver.kind
    else {
        diagnostics.push(Diagnostic::error(
            "alpha typecheck does not yet support instance method calls".to_string(),
            receiver.span,
        ));
        return ResolvedType::unresolved();
    };

    let receiver_path = [receiver_name.clone()];
    let Some((struct_id, struct_entry)) = lookup_struct(&receiver_path, package, registry) else {
        diagnostics.push(Diagnostic::error(
            "alpha typecheck does not yet support instance method calls".to_string(),
            receiver.span,
        ));
        return ResolvedType::unresolved();
    };
    if !matches!(struct_entry.kind, GlobalKind::Struct(_)) {
        diagnostics.push(Diagnostic::error(
            "alpha typecheck does not yet support instance method calls".to_string(),
            receiver.span,
        ));
        return ResolvedType::unresolved();
    }

    *receiver_resolution = Resolution::Global(struct_id);
    // Stamp the receiver's *outer* Expr.resolution as well so seal
    // can walk it as a regular Ident reference (the Call carve-out
    // for `Unresolved` callees doesn't apply here — the receiver of
    // a static method call is "the struct type", which is a stable
    // referent the same way any other type-leaf is).
    receiver.resolution = ResolvedType::leaf(Resolution::Global(struct_id));

    let mut method_path = struct_entry.identifier.path().to_vec();
    method_path.push(method.to_string());
    let method_identifier = Identifier::new(struct_entry.identifier.package(), method_path);
    let Some((_, method_entry)) = registry.lookup(&method_identifier) else {
        diagnostics.push(Diagnostic::error(
            format!(
                "`{}` has no static method `{method}`",
                struct_entry.identifier,
            ),
            call_span,
        ));
        return ResolvedType::unresolved();
    };
    let sig = match &method_entry.kind {
        GlobalKind::Function(Some(sig)) => sig,
        GlobalKind::Function(None) => panic!(
            "resolve_method_call: method `{}` has no lifted signature — \
             lift_signatures must run before resolve",
            method_entry.identifier,
        ),
        other => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "cannot call `{}.{method}`: it is a {}, not a function",
                    struct_entry.identifier,
                    other.label(),
                ),
                call_span,
            ));
            return ResolvedType::unresolved();
        }
    };

    validate_arg_signature(
        args,
        sig,
        &method_entry.identifier,
        call_span,
        registry,
        diagnostics,
    );
    sig.return_type.clone()
}

/// Resolve every call/method-call argument expression. Named
/// arguments diagnose up front so nested resolution still proceeds
/// (gives `seal_expr` a populated tree to walk).
fn resolve_args(
    args: &mut [Arg],
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for arg in args.iter_mut() {
        if let Some(name) = arg.name.as_ref() {
            diagnostics.push(Diagnostic::error(
                format!("alpha typecheck does not yet support named arguments (got `{name}`)",),
                arg.span,
            ));
        }
        resolve_expr(&mut arg.value, package, registry, diagnostics);
    }
}

/// Check argument arity + per-position type compatibility against a
/// lifted [`FunctionSignature`]. Diagnostics use the callee's
/// fully-qualified [`Identifier`] so the user sees `TestApp.Point.at`
/// rather than just `at`.
fn validate_arg_signature(
    args: &[Arg],
    sig: &FunctionSignature,
    callee: &Identifier,
    call_span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if args.len() != sig.params.len() {
        diagnostics.push(Diagnostic::error(
            format!(
                "`{callee}` expects {} argument{}, got {}",
                sig.params.len(),
                if sig.params.len() == 1 { "" } else { "s" },
                args.len(),
            ),
            call_span,
        ));
        return;
    }

    for (arg, param) in args.iter().zip(sig.params.iter()) {
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

fn resolve_string(
    parts: &[StringPart],
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    if parts
        .iter()
        .any(|part| matches!(part, StringPart::Interpolation { .. }))
    {
        diagnostics.push(Diagnostic::error(
            "alpha typecheck does not yet support string interpolation",
            span,
        ));
        return ResolvedType::unresolved();
    }
    registry.primitive("String")
}

/// Resolve a struct literal `Type{f1: e1, f2: e2}`. Validates that
/// the type path resolves to a registered struct, that every
/// declared field has exactly one init with a matching type, and
/// that no unknown fields appear. Returns the struct's `ResolvedType`
/// regardless of arg-mismatch failures so the surrounding expression
/// keeps a stable shape — individual field-mismatch diagnostics
/// surface separately.
///
/// Move tracking is deferred: the surface-syntax `move` modifier on
/// fields is rejected upstream by the parser/AST (no shape exists),
/// and field reads (resolved separately in [`resolve_field_access`])
/// don't invalidate the receiver. This matches v1's current
/// behaviour. Tightening lands with the ownership slice.
fn resolve_struct_construction(
    type_path: &[String],
    fields: &mut [FieldInit],
    span: Span,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    // Resolve every field-init expression first regardless of struct
    // resolution success — nested errors surface and seal_expr has
    // resolutions to walk on each value.
    for field in fields.iter_mut() {
        resolve_expr(&mut field.value, package, registry, diagnostics);
    }

    let Some((struct_id, struct_entry)) = lookup_struct(type_path, package, registry) else {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not recognize the struct type `{}`",
                type_path.join("."),
            ),
            span,
        ));
        return ResolvedType::unresolved();
    };

    let GlobalKind::Struct(definition) = &struct_entry.kind else {
        diagnostics.push(Diagnostic::error(
            format!(
                "cannot construct `{}`: it is a {}, not a struct",
                struct_entry.identifier,
                struct_entry.kind.label(),
            ),
            span,
        ));
        return ResolvedType::unresolved();
    };
    let Some(definition) = definition else {
        // Stdlib stub primitives and other Struct(None) entries are
        // not user-constructible. Diagnose distinctly so the user
        // gets a clearer hint than "unknown field".
        diagnostics.push(Diagnostic::error(
            format!(
                "cannot construct primitive type `{}` with struct literal syntax",
                struct_entry.identifier,
            ),
            span,
        ));
        return ResolvedType::leaf(Resolution::Global(struct_id));
    };

    let struct_name = struct_entry.identifier.clone();
    validate_struct_fields(
        &struct_name,
        definition,
        fields,
        span,
        registry,
        diagnostics,
    );

    ResolvedType::leaf(Resolution::Global(struct_id))
}

fn validate_struct_fields(
    struct_name: &Identifier,
    definition: &StructDefinition,
    fields: &[FieldInit],
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut seen: Vec<bool> = vec![false; definition.fields.len()];
    for field in fields {
        let Some((index, declared)) = definition.lookup_field(&field.name) else {
            diagnostics.push(Diagnostic::error(
                format!("`{struct_name}` has no field `{}`", field.name,),
                field.span,
            ));
            continue;
        };
        if seen[index as usize] {
            diagnostics.push(Diagnostic::error(
                format!(
                    "field `{}` of `{struct_name}` initialized twice",
                    field.name
                ),
                field.span,
            ));
            continue;
        }
        seen[index as usize] = true;

        let actual = &field.value.resolution;
        if !actual.is_resolved() {
            // The init expression already triggered its own
            // diagnostic; don't pile on with a type mismatch.
            continue;
        }
        if actual != &declared.ty {
            diagnostics.push(Diagnostic::error(
                format!(
                    "field `{}` of `{struct_name}` expects `{}`, got `{}`",
                    field.name,
                    display_resolution(&declared.ty, registry),
                    display_resolution(actual, registry),
                ),
                field.span,
            ));
        }
    }
    for (index, present) in seen.iter().enumerate() {
        if !*present {
            diagnostics.push(Diagnostic::error(
                format!(
                    "missing field `{}` in struct literal for `{struct_name}`",
                    definition.fields[index].name,
                ),
                span,
            ));
        }
    }
}

fn resolve_field_access(
    receiver: &mut Expr,
    field: &str,
    span: Span,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_expr(receiver, package, registry, diagnostics);
    let Resolution::Global(struct_id) = receiver.resolution.resolution else {
        // Receiver resolution failed upstream; stay quiet to avoid
        // duplicating that diagnostic.
        return ResolvedType::unresolved();
    };
    let Some(entry) = registry.get(struct_id) else {
        return ResolvedType::unresolved();
    };
    let GlobalKind::Struct(Some(definition)) = &entry.kind else {
        diagnostics.push(Diagnostic::error(
            format!(
                "field access requires a struct receiver; got `{}` ({})",
                entry.identifier,
                entry.kind.label(),
            ),
            span,
        ));
        return ResolvedType::unresolved();
    };
    let Some((_, declared)) = definition.lookup_field(field) else {
        diagnostics.push(Diagnostic::error(
            format!("`{}` has no field `{field}`", entry.identifier),
            span,
        ));
        return ResolvedType::unresolved();
    };
    declared.ty.clone()
}

fn lookup_struct<'a>(
    type_path: &[String],
    package: &str,
    registry: &'a GlobalRegistry,
) -> Option<(GlobalRegistryId, &'a RegistryEntry)> {
    if type_path.len() != 1 {
        return None;
    }
    let name = &type_path[0];
    if let Some(found) = registry.lookup(&Identifier::new(package, vec![name.clone()])) {
        return Some(found);
    }
    registry.lookup(&Identifier::new("Global", vec![name.clone()]))
}
