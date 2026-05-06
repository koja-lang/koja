//! Struct-literal construction and field-access resolution.
//!
//! Struct construction (`Type{f: e}`) validates that every declared
//! field has exactly one init with a matching type and no unknown /
//! duplicate fields surface; the literal's [`ResolvedType`] is the
//! struct's leaf type regardless of per-field mismatches so the
//! surrounding expression keeps a stable shape — individual
//! diagnostics fire per offending field.
//!
//! Field access (`expr.field`) requires the receiver to resolve to a
//! struct value; the result type is the declared field's type.
//!
//! `lookup_struct` is shared with [`super::calls`] for the
//! `Type.method(...)` static-dispatch carve-out — both helpers need
//! the same package + Global fallback.

use expo_ast::ast::{Diagnostic, Expr, FieldInit};
use expo_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

use crate::registry::{GlobalKind, GlobalRegistry, RegistryEntry, StructDefinition};

use super::ctx::Resolver;
use super::expr::resolve_expr;
use super::types::display_resolution;

/// Resolve a struct literal `Type{f1: e1, f2: e2}`. Validates that
/// the type path resolves to a registered struct, that every
/// declared field has exactly one init with a matching type, and
/// that no unknown fields appear.
///
/// Move tracking is deferred: the surface-syntax `move` modifier on
/// fields is rejected upstream by the parser/AST (no shape exists),
/// and field reads (resolved separately by [`resolve_field_access`])
/// don't invalidate the receiver. This matches v1's current
/// behaviour. Tightening lands with the ownership slice.
pub(super) fn resolve_struct_construction(
    type_path: &[String],
    fields: &mut [FieldInit],
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    // Resolve every field-init expression first regardless of struct
    // resolution success — nested errors surface and seal_expr has
    // resolutions to walk on each value.
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

pub(super) fn resolve_field_access(
    receiver: &mut Expr,
    field: &str,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    resolve_expr(receiver, resolver, diagnostics);
    let Resolution::Global(struct_id) = receiver.resolution.resolution else {
        // Receiver resolution failed upstream; stay quiet to avoid
        // duplicating that diagnostic.
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
/// falling back to `Global` for stdlib stubs (`Int`, `Bool`, …).
/// Multi-segment paths and aliases aren't supported in this slice.
///
/// `pub(super)` so [`super::calls`] can reuse the same lookup for
/// `Type.method(args)` static dispatch.
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
