//! Struct-literal construction and field-access resolution. Also
//! owns `lookup_type` (used by [`super::calls`] for static dispatch
//! and by [`super::enums`] for enum-variant construction) and
//! `validate_named_fields` (the shared name/type-checked field-init
//! walk used by both struct construction and struct-variant
//! construction). Structs own the "named field layout" concept; the
//! enum module imports rather than duplicating.

use expo_ast::ast::{Diagnostic, Expr, FieldInit};
use expo_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

use crate::pipeline::unify::{Conflict, substitute_resolved_type, unify_resolved_type};
use crate::registry::{GlobalKind, GlobalRegistry, RegistryEntry, ResolvedStructField};

use super::ctx::{Callee, Resolver};
use super::expr::resolve_expr;
use super::types::display_resolution;

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

    let Some((struct_id, struct_entry)) =
        lookup_type(type_path, resolver.package, resolver.registry)
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
        diagnostics.push(Diagnostic::error(
            format!(
                "cannot construct primitive type `{}` with struct literal syntax",
                struct_entry.identifier,
            ),
            span,
        ));
        return ResolvedType::leaf(Resolution::Global(struct_id));
    };

    let owner = struct_entry.identifier.to_string();
    let type_params = struct_entry.type_params.clone();
    if type_params.is_empty() {
        validate_named_fields(
            &owner,
            &definition.fields,
            fields,
            span,
            resolver.registry,
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
            ty: substitute_resolved_type(&field.ty, &subst, struct_id),
        })
        .collect();
    validate_named_fields(
        &owner,
        &substituted_fields,
        fields,
        span,
        resolver.registry,
        diagnostics,
    );
    let type_args = subst
        .into_iter()
        .map(|slot| slot.unwrap_or_else(ResolvedType::unresolved))
        .collect();
    ResolvedType {
        resolution: Resolution::Global(struct_id),
        type_args,
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
) -> Vec<Option<ResolvedType>> {
    let mut subst: Vec<Option<ResolvedType>> = vec![None; callee.type_params.len()];
    for field in fields {
        let Some((_, declared_field)) = lookup_named_field(declared, &field.name) else {
            continue;
        };
        if !field.value.resolution.is_resolved() {
            continue;
        }
        if let Err(conflict) = unify_resolved_type(
            &declared_field.ty,
            &field.value.resolution,
            callee.id,
            &mut subst,
        ) {
            emit_conflict(&callee, conflict, field.span, registry, diagnostics);
        }
    }
    for (index, slot) in subst.iter().enumerate() {
        if slot.is_none() {
            diagnostics.push(Diagnostic::error(
                format!(
                    "alpha typecheck cannot infer type parameter `{}` of `{}` \
                     from the supplied fields",
                    callee.type_params[index], callee.label,
                ),
                span,
            ));
        }
    }
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
    fields: &[FieldInit],
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut seen: Vec<bool> = vec![false; declared.len()];
    for field in fields {
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
        if actual != &declared_field.ty {
            diagnostics.push(Diagnostic::error(
                format!(
                    "field `{}` of `{owner_label}` expects `{}`, got `{}`",
                    field.name,
                    display_resolution(&declared_field.ty, registry),
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
    let Resolution::Global(struct_id) = receiver.resolution.resolution else {
        return ResolvedType::unresolved();
    };
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
    declared.ty.clone()
}

/// Resolve a single-segment type path against the in-scope package,
/// falling back to `Global` for stdlib stubs. Multi-segment paths
/// and aliases are feature gaps.
///
/// Generalized from the struct-only `lookup_struct` so enum-variant
/// construction (`Color.Red`) and static method dispatch on enums
/// (`Color.method(...)`) can share the same path-resolution logic.
/// Callers narrow on `entry.kind` if they care about kind.
pub(super) fn lookup_type<'a>(
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
