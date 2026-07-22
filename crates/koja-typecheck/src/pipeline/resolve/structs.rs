//! Struct-literal construction and field-access resolution. Owns
//! `validate_named_fields` (the shared name/type-checked field-init
//! walk used by both struct construction and struct-variant
//! construction). Structs own the "named field layout" concept,
//! and [`super::enums`] imports it rather than duplicating. The
//! cross-cutting `lookup_type` registry helper lives one module
//! over in [`super::types`] alongside the other registry-backed
//! type predicates.

use koja_ast::ast::{Diagnostic, EnumConstructionData, Expr, ExprKind, FieldInit};
use koja_ast::identifier::{
    AnonymousKind, GlobalRegistryId, Identifier, Resolution, ResolvedType, TypeParamIndex,
};
use koja_ast::span::Span;

use crate::pipeline::unify::{Conflict, Substitution, substitute};
use crate::pipeline::visibility::check_reference_visibility;
use crate::registry::{GlobalKind, GlobalRegistry, ResolvedStructField};

use super::coercion::{Mismatch, check_compatible_stamping};
use super::ctx::{Callee, Resolver};
use super::expr::{resolve_expr, resolve_expr_with_expected};
use super::inference::{PhantomContext, fill_from_expected, finalize_inference, unify_pairs};
use super::types::{display_resolution, lookup_type, names_struct, peel_alias};

/// `A.B { … }` parses as a struct-shaped enum-variant construction. If
/// the full path names a struct, rewrite it in place to a
/// `StructConstruction` so [`resolve_struct_construction`] handles it.
pub(super) fn rewrite_dotted_struct_construction(expr: &mut Expr, resolver: &Resolver<'_>) {
    let ExprKind::EnumConstruction {
        type_path,
        variant,
        data: EnumConstructionData::Struct(_),
    } = &expr.kind
    else {
        return;
    };
    if !names_struct(&joined(type_path, variant), resolver.resolution_scope()) {
        return;
    }
    let ExprKind::EnumConstruction {
        mut type_path,
        variant,
        data: EnumConstructionData::Struct(fields),
    } = std::mem::replace(&mut expr.kind, ExprKind::Self_ { local_id: None })
    else {
        unreachable!("guarded by the match above");
    };
    type_path.push(variant);
    expr.kind = ExprKind::StructConstruction { type_path, fields };
}

/// `type_path ++ [variant]`.
fn joined(type_path: &[String], variant: &str) -> Vec<String> {
    let mut path = type_path.to_vec();
    path.push(variant.to_string());
    path
}

/// Resolve `Type{f1: e1, f2: e2}`. Validates the type path resolves
/// to a registered struct, every declared field has exactly one init
/// of the right type, and no unknown fields appear. Threads
/// `expected` (the surrounding context's type hint) down so generic
/// type-args are seeded from outside and `Option.None` / `[]` /
/// nested struct-literals in field positions get the declared field
/// type as their own expected hint. The literal's [`ResolvedType`]
/// is always the struct's leaf type regardless of per-field
/// mismatches so the surrounding expression stays stable.
pub(super) fn resolve_struct_construction(
    type_path: &[String],
    fields: &mut [FieldInit],
    expected: Option<&ResolvedType>,
    span: Span,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let Some((struct_id, struct_entry)) = lookup_type(type_path, resolver.resolution_scope())
    else {
        bare_walk_fields(fields, resolver, diagnostics);
        diagnostics.push(Diagnostic::error(
            format!(
                "typecheck does not recognize the struct type `{}`",
                type_path.join("."),
            ),
            span,
        ));
        return ResolvedType::unresolved();
    };
    check_reference_visibility(struct_entry, resolver.package, span, diagnostics);

    let GlobalKind::Struct(definition) = &struct_entry.kind else {
        bare_walk_fields(fields, resolver, diagnostics);
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
            "typecheck: struct entry `{}` reached struct-literal validation \
             without a stamped definition: every struct (including stdlib stubs) \
             carries `Struct(Some(_))` after preload",
            struct_entry.identifier,
        );
    };
    if is_unconstructable_primitive(&struct_entry.identifier) {
        bare_walk_fields(fields, resolver, diagnostics);
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
        walk_field_inits(&definition.fields, fields, resolver, diagnostics);
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
    let seeded_subst = seed_subst_from_expected(callee, expected, resolver.registry);
    let substituted_fields = substitute_declared_fields(&definition.fields, &seeded_subst);
    walk_field_inits(&substituted_fields, fields, resolver, diagnostics);
    let subst = infer_struct_type_args(
        callee,
        &definition.fields,
        fields,
        seeded_subst,
        span,
        resolver.registry,
        diagnostics,
    );
    let substituted_fields = substitute_declared_fields(&definition.fields, &subst);
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

/// Resolve every field-init expression with no expected hint, a
/// fallback for paths where the struct itself failed to resolve.
/// Keeps the seal pass walking a populated tree.
fn bare_walk_fields(
    fields: &mut [FieldInit],
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for field in fields.iter_mut() {
        resolve_expr(&mut field.value, resolver, diagnostics);
    }
}

/// Walk a `FieldInit` list against a substituted declared roster:
/// for each init, look up the matching declared field and resolve
/// its value with that field's type as the expected hint. Inits
/// whose name doesn't match any declared field fall through to the
/// bare walk (the unknown-field diagnostic in
/// [`validate_named_fields`] reports them). Shared by struct
/// construction and the struct-variant arm of enum construction.
pub(super) fn walk_field_inits(
    declared: &[ResolvedStructField],
    fields: &mut [FieldInit],
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for field in fields.iter_mut() {
        let Some((_, declared_field)) = lookup_named_field(declared, &field.name) else {
            resolve_expr(&mut field.value, resolver, diagnostics);
            continue;
        };
        resolve_expr_with_expected(
            &mut field.value,
            Some(&declared_field.ty),
            resolver,
            diagnostics,
        );
    }
}

fn substitute_declared_fields(
    declared: &[ResolvedStructField],
    subst: &Substitution,
) -> Vec<ResolvedStructField> {
    declared
        .iter()
        .map(|field| ResolvedStructField {
            name: field.name.clone(),
            ty: substitute(&field.ty, subst),
        })
        .collect()
}

/// Pre-seed an empty substitution from the surrounding context's
/// expected type, mirroring the enum-side
/// [`super::enums::resolve_enum_construction`] flow. Lets
/// `pair: Pair<Int, Bool> = Pair{...}` pin `T` / `U` before any
/// field's value is even seen, so field-init walks see fully
/// substituted declared types.
fn seed_subst_from_expected(
    callee: Callee<'_>,
    expected: Option<&ResolvedType>,
    registry: &GlobalRegistry,
) -> Substitution {
    let mut subst = Substitution::single(callee.id, callee.type_params.len());
    if let Some(hint) = expected {
        let template = canonical_struct_template(callee.id, callee.type_params.len());
        fill_from_expected(&template, hint, &mut subst, registry);
    }
    subst
}

/// Build the struct's canonical self-referential template
/// `Named { Global(struct_id), [TypeParam(struct_id, 0..N)] }`.
/// Mirrors `canonical_enum_template` in [`super::enums`].
fn canonical_struct_template(struct_id: GlobalRegistryId, arity: usize) -> ResolvedType {
    ResolvedType::Named {
        resolution: Resolution::Global(struct_id),
        type_args: (0..arity)
            .map(|index| ResolvedType::Named {
                resolution: Resolution::TypeParam {
                    owner: struct_id,
                    index: TypeParamIndex::new(index as u32),
                },
                type_args: Vec::new(),
            })
            .collect(),
    }
}

/// Infer concrete `type_args` for a generic struct construction by
/// unifying each declared field's template type against the resolved
/// type of its corresponding field-init value. `seeded` is the
/// substitution after any bidirectional fill from the surrounding
/// expected type. Payload-driven unification adds to it without
/// overriding existing bindings. Emits one diagnostic per
/// [`Conflict`] (T inferred to two distinct types) and one per
/// phantom param (no field nor outer hint constrains it).
fn infer_struct_type_args(
    callee: Callee<'_>,
    declared: &[ResolvedStructField],
    fields: &[FieldInit],
    seeded: Substitution,
    span: Span,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Substitution {
    let mut subst = seeded;
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
/// construction and enum struct-variant construction. Both share
/// the same shape and the same diagnostic surface (unknown field,
/// duplicate initialization, missing field, wrong-typed init).
///
/// `owner_label` is the prefix used in diagnostics
/// (`MyApp.MyStruct` for structs, `MyApp.MyEnum.MyVariant` for
/// enum struct variants). Each `FieldInit.value` must already have
/// `resolution` populated (either resolved or `Unresolved`). Inits
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

        let actual = field.value.resolution.clone();
        if !actual.is_resolved() {
            continue;
        }
        match check_compatible_stamping(
            &mut field.value,
            &actual,
            &declared_field.ty,
            resolver.registry,
        ) {
            None => {}
            Some(Mismatch::OutOfRange {
                rendered_value,
                width,
            }) => {
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
            Some(Mismatch::Incompatible) => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "field `{}` of `{owner_label}` expects `{}`, got `{}`",
                        field.name,
                        display_resolution(&declared_field.ty, resolver.registry),
                        display_resolution(&actual, resolver.registry),
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
    match peel_alias(&receiver.resolution, resolver.registry) {
        ResolvedType::Union(_) => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "cannot access field `{field}` on union type `{}`. \
                     Match the union first to bind a specific variant",
                    display_resolution(&receiver.resolution, resolver.registry),
                ),
                span,
            ));
            return ResolvedType::unresolved();
        }
        ResolvedType::Anonymous(AnonymousKind::Tuple { .. }) => {
            diagnostics.push(Diagnostic::error_with_hint(
                format!(
                    "cannot access field `{field}` on tuple `{}`",
                    display_resolution(&receiver.resolution, resolver.registry),
                ),
                "tuples have no named or positional fields. Destructure \
                 instead with `(a, b) = value`",
                span,
            ));
            return ResolvedType::unresolved();
        }
        _ => {}
    }
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
                "field access requires a struct receiver, got `{}` ({})",
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
    // empty and substitution is a no-op. For generic-but-aliased
    // receivers (`self: Bag<TypeParam(Bag, 0)>` inside an inherent
    // method on `struct Bag<T>`) the field type's `TypeParam`
    // round-trips back to itself.
    let subst = Substitution::from_args(struct_id, receiver_args);
    substitute(&declared.ty, &subst)
}

/// `Global.<name>` types whose values are produced exclusively by
/// literals, intrinsics, or arithmetic, never by struct-literal
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
