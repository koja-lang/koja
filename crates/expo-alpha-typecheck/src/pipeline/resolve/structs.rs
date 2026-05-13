//! Struct-literal construction and field-access resolution. Owns
//! `validate_named_fields` (the shared name/type-checked field-init
//! walk used by both struct construction and struct-variant
//! construction) — structs own the "named field layout" concept,
//! and [`super::enums`] imports it rather than duplicating. The
//! cross-cutting `lookup_type` registry helper lives one module
//! over in [`super::types`] alongside the other registry-backed
//! type predicates.

use expo_ast::ast::{Diagnostic, Expr, FieldInit};
use expo_ast::coercion::LiteralCoercion;
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

use crate::pipeline::unify::{Conflict, Substitution, substitute};
use crate::registry::{GlobalKind, GlobalRegistry, ResolvedStructField};

use super::coercion::{Compatible, check_compatible, coercion_target_mut};
use super::ctx::{Callee, Resolver};
use super::expr::resolve_expr;
use super::inference::{PhantomContext, finalize_inference, unify_pairs};
use super::types::{display_resolution, lookup_type};

/// Resolve `Type{f1: e1, f2: e2}`. Validates the type path resolves
/// to a registered struct, every declared field has exactly one init
/// of the right type, and no unknown fields appear. The literal's
/// [`ResolvedType`] is always the struct's leaf type regardless of
/// per-field mismatches so the surrounding expression stays stable.
pub(super) fn resolve_struct_construction(
    type_path: &[String],
    fields: &mut [FieldInit],
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    // Resolve every field-init expression up front so seal walks a
    // populated tree even if the struct itself doesn't resolve.
    for field in fields.iter_mut() {
        resolve_expr(&mut field.value, resolver, diagnostics);
    }

    let Some((struct_id, struct_entry)) = lookup_type(type_path, resolver.resolution_scope())
    else {
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
        panic!(
            "alpha typecheck: struct entry `{}` reached struct-literal validation \
             without a stamped definition — every struct (including stdlib stubs) \
             carries `Struct(Some(_))` after preload",
            struct_entry.identifier,
        );
    };
    if is_unconstructable_primitive(&struct_entry.identifier) {
        diagnostics.push(Diagnostic::error(
            format!(
                "cannot construct primitive type `{}` with struct literal syntax",
                struct_entry.identifier,
            ),
            span,
        ));
        return ResolvedType::leaf(Resolution::Global(struct_id));
    }

    let owner = struct_entry.identifier.to_string();
    let type_params = struct_entry.type_params.clone();
    if type_params.is_empty() {
        validate_named_fields(
            &owner,
            &definition.fields,
            fields,
            span,
            resolver,
            diagnostics,
        );
        return ResolvedType::leaf(Resolution::Global(struct_id));
    }

    let callee = Callee {
        id: struct_id,
        label: &owner,
        type_params: &type_params,
    };
    let subst = infer_struct_type_args(
        callee,
        &definition.fields,
        fields,
        span,
        resolver.registry,
        diagnostics,
    );
    let substituted_fields: Vec<ResolvedStructField> = definition
        .fields
        .iter()
        .map(|field| ResolvedStructField {
            name: field.name.clone(),
            ty: substitute(&field.ty, &subst),
        })
        .collect();
    validate_named_fields(
        &owner,
        &substituted_fields,
        fields,
        span,
        resolver,
        diagnostics,
    );
    ResolvedType::Named {
        resolution: Resolution::Global(struct_id),
        type_args: subst.args(struct_id),
    }
}

/// Infer concrete `type_args` for a generic struct construction by
/// unifying each declared field's template type against the resolved
/// type of its corresponding field-init value. Emits one diagnostic
/// per [`Conflict`] (T inferred to two distinct types) and one per
/// Phantom param (no field constrains it). Slots without inference
/// stay `None` so the caller surfaces an unresolved leaf.
fn infer_struct_type_args(
    callee: Callee<'_>,
    declared: &[ResolvedStructField],
    fields: &[FieldInit],
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Substitution {
    let mut subst = Substitution::single(callee.id, callee.type_params.len());
    let pairs = fields.iter().filter_map(|field| {
        let (_, declared_field) = lookup_named_field(declared, &field.name)?;
        Some((&declared_field.ty, &field.value.resolution, field.span))
    });
    unify_pairs(pairs, &mut subst, registry, |conflict, field_span| {
        emit_conflict(&callee, conflict, field_span, registry, diagnostics);
    });
    finalize_inference(
        &[callee],
        &subst,
        &PhantomContext::Fields,
        span,
        registry,
        diagnostics,
    );
    subst
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

/// Validate a [`FieldInit`] list against a declared
/// [`ResolvedStructField`] roster. Shared by struct literal
/// construction and enum struct-variant construction — both share
/// the same shape and the same diagnostic surface (unknown field,
/// duplicate initialization, missing field, wrong-typed init).
///
/// `owner_label` is the prefix used in diagnostics
/// (`MyApp.MyStruct` for structs, `MyApp.MyEnum.MyVariant` for
/// enum struct variants). Each `FieldInit.value` must already have
/// `resolution` populated (either resolved or `Unresolved`); inits
/// with unresolved values skip the type-equality check (their own
/// upstream diagnostic already fired).
pub(super) fn validate_named_fields(
    owner_label: &str,
    declared: &[ResolvedStructField],
    fields: &mut [FieldInit],
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut seen: Vec<bool> = vec![false; declared.len()];
    for field in fields.iter_mut() {
        let Some((index, declared_field)) = lookup_named_field(declared, &field.name) else {
            diagnostics.push(Diagnostic::error(
                format!("`{owner_label}` has no field `{}`", field.name),
                field.span,
            ));
            continue;
        };
        if seen[index as usize] {
            diagnostics.push(Diagnostic::error(
                format!(
                    "field `{}` of `{owner_label}` initialized twice",
                    field.name
                ),
                field.span,
            ));
            continue;
        }
        seen[index as usize] = true;

        let actual = &field.value.resolution;
        if !actual.is_resolved() {
            continue;
        }
        match check_compatible(&field.value, actual, &declared_field.ty, resolver.registry) {
            Compatible::Strict => {}
            Compatible::Coerced(width) => {
                *coercion_target_mut(&mut field.value) =
                    Some(LiteralCoercion::NumericLiteralWidth(width));
            }
            Compatible::OutOfRange {
                rendered_value,
                width,
            } => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "field `{}` of `{owner_label}` expects `{}`: value \
                         `{rendered_value}` does not fit in `{}` (range {})",
                        field.name,
                        display_resolution(&declared_field.ty, resolver.registry),
                        width.label(),
                        width.range_label(),
                    ),
                    field.span,
                ));
            }
            Compatible::Incompatible => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "field `{}` of `{owner_label}` expects `{}`, got `{}`",
                        field.name,
                        display_resolution(&declared_field.ty, resolver.registry),
                        display_resolution(actual, resolver.registry),
                    ),
                    field.span,
                ));
            }
        }
    }
    for (index, present) in seen.iter().enumerate() {
        if !*present {
            diagnostics.push(Diagnostic::error(
                format!(
                    "missing field `{}` in literal for `{owner_label}`",
                    declared[index].name,
                ),
                span,
            ));
        }
    }
}

fn lookup_named_field<'a>(
    declared: &'a [ResolvedStructField],
    name: &str,
) -> Option<(u32, &'a ResolvedStructField)> {
    declared
        .iter()
        .enumerate()
        .find(|(_, field)| field.name == name)
        .map(|(index, field)| (index as u32, field))
}

pub(super) fn resolve_field_access(
    receiver: &mut Expr,
    field: &str,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_expr(receiver, resolver, diagnostics);
    let ResolvedType::Named {
        resolution: Resolution::Global(struct_id),
        type_args: receiver_args,
    } = &receiver.resolution
    else {
        return ResolvedType::unresolved();
    };
    let struct_id = *struct_id;
    let Some(entry) = resolver.registry.get(struct_id) else {
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
    // Substitute the field's declared type through the receiver's
    // type-args so `self.item` on `Bag<Int>` types as `Int`, not
    // `TypeParam(Bag, 0)`. For non-generic structs `type_args` is
    // empty and substitution is a no-op; for generic-but-aliased
    // receivers (`self: Bag<TypeParam(Bag, 0)>` inside an inherent
    // method on `struct Bag<T>`) the field type's `TypeParam`
    // round-trips back to itself.
    let subst = Substitution::from_args(struct_id, receiver_args);
    substitute(&declared.ty, &subst)
}

/// `Global.<name>` types whose values are produced exclusively by
/// literals, intrinsics, or arithmetic — never by struct-literal
/// syntax. The list mirrors the preloaded primitive stubs in
/// [`crate::registry::GlobalRegistry::with_stdlib_stubs`].
fn is_unconstructable_primitive(identifier: &Identifier) -> bool {
    if !identifier.is_in_global() {
        return false;
    }
    matches!(
        identifier.last(),
        "Binary"
            | "Bits"
            | "Bool"
            | "CPtr"
            | "Float"
            | "Float32"
            | "Float64"
            | "Int"
            | "Int16"
            | "Int32"
            | "Int64"
            | "Int8"
            | "Never"
            | "String"
            | "UInt16"
            | "UInt32"
            | "UInt64"
            | "UInt8"
            | "Unit"
    )
}
