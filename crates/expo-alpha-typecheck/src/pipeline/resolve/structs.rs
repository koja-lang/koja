//! Struct-literal construction and field-access resolution.
//! `lookup_struct` is also reused by [`super::calls`] for static
//! dispatch (`Type.method(...)`).

use expo_ast::ast::{Diagnostic, Expr, FieldInit};
use expo_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

use crate::registry::{GlobalKind, GlobalRegistry, RegistryEntry, StructDefinition};

use super::ctx::Resolver;
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
        lookup_struct(type_path, resolver.package, resolver.registry)
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

    let struct_name = struct_entry.identifier.clone();
    validate_struct_fields(
        &struct_name,
        definition,
        fields,
        span,
        resolver.registry,
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

/// Resolve a single-segment struct path against the in-scope package,
/// falling back to `Global` for stdlib stubs. Multi-segment paths
/// and aliases are feature gaps.
pub(super) fn lookup_struct<'a>(
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
