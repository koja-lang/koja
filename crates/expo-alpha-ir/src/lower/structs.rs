//! Struct-shaped lowering: declarations, struct-literal construction,
//! and field reads. Mirrors the layout of [`super::control_flow`] and
//! [`super::ops`] — one helper per AST shape.
//!
//! Decl lowering pulls the canonical field layout off the typecheck
//! registry's [`GlobalKind::Struct(Some(definition))`] so we never
//! re-resolve a `TypeExpr` here. Construction and field access do
//! the same: typecheck has already validated names and types, so
//! IR's job is purely "stamp positional indices and the resolved
//! per-field [`IRType`] onto the instruction".
//!
//! Move tracking is deferred — field reads produce a value of the
//! field's IRType without invalidating the receiver, matching v1
//! and the alpha resolve sub-pass. Tightening lands with the
//! ownership slice.

use std::collections::BTreeMap;

use expo_alpha_typecheck::{
    GlobalKind, GlobalRegistry, RegistryEntry, ResolvedStructField, StructDefinition,
};
use expo_ast::ast::{Diagnostic, Expr, FieldInit, StructDecl, StructField};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};

use crate::function::{IRBlockId, IRInstruction, IRSymbol};
use crate::struct_decl::{IRStructDecl, IRStructField, StructFieldInit};
use crate::types::{IRType, ValueId};

use super::ctx::FnLowerCtx;
use super::expr::lower_expr;
use super::package::resolved_type_to_ir_type;

/// Lower an `Item::Struct` against the typecheck registry. Returns
/// `None` if any feature-gap diagnostic surfaced (matches the
/// per-function fail-fast contract: the offending decl is dropped
/// from the package). The pre-lifted [`GlobalKind::Struct`] entry on
/// the registry already carries the canonical field layout — this
/// helper just stamps the positional indices and translates each
/// field's [`expo_alpha_typecheck::ResolvedStructField::ty`] into an
/// [`IRType`].
pub(super) fn lower_struct_decl(
    decl: &StructDecl,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<IRStructDecl> {
    if has_feature_gap(decl, diagnostics) {
        return None;
    }
    let identifier = Identifier::new(package, vec![decl.name.clone()]);
    let entry = registry.lookup(&identifier).map(|(_, entry)| entry)?;
    let GlobalKind::Struct(Some(definition)) = &entry.kind else {
        panic!(
            "alpha IR lower: struct `{identifier}` has no lifted definition — \
             lift_signatures invariant violation",
        );
    };
    let symbol = IRSymbol::from_identifier(&entry.identifier);
    let mut fields = Vec::with_capacity(definition.fields.len());
    for (index, declared) in definition.fields.iter().enumerate() {
        let ir_type = resolved_type_to_ir_type(&declared.ty, registry);
        fields.push(IRStructField {
            index: index as u32,
            ir_type,
            name: declared.name.clone(),
        });
    }
    Some(IRStructDecl { fields, symbol })
}

/// Lower a struct literal `Type{name: value, ...}`. Diagnoses the
/// resolved struct id off the call site's `expr.resolution` and
/// canonicalizes the field-init list to declaration order so seal /
/// backends iterate linearly. Each value is lowered through
/// [`lower_expr`]; AST field-init order doesn't bleed into the IR.
pub(super) fn lower_struct_construction(
    fields: &[FieldInit],
    expr_resolution: &ResolvedType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(ValueId, IRBlockId), ()> {
    let definition = struct_definition_from_resolution(expr_resolution, registry, "construction");
    let entry = struct_entry_from_resolution(expr_resolution, registry, "construction");
    let symbol = IRSymbol::from_identifier(&entry.identifier);

    let (field_inits, current) = canonicalize_struct_inits(
        &definition.fields,
        fields,
        ctx,
        block,
        registry,
        diagnostics,
    )?;

    let dest = ctx.fresh_value(IRType::Struct(symbol.clone()));
    ctx.cfg.append(
        current,
        IRInstruction::StructInit {
            dest,
            fields: field_inits,
            ty: symbol,
        },
    );
    Ok((dest, current))
}

/// Lower each field-init expression and re-order the results into
/// declaration order. Shared by struct-literal construction and
/// enum struct-variant construction — both want the same
/// "lower in source order, threading control flow, then canonicalize
/// to declaration order" pipeline. The struct slice owns the helper
/// because structs own the "named field layout" concept; the enum
/// lowering module imports.
///
/// Panics if a declared field has no corresponding init (typecheck
/// seal forbids this; reaching it here is an invariant violation,
/// not a feature gap).
pub(super) fn canonicalize_struct_inits(
    declared: &[ResolvedStructField],
    fields: &[FieldInit],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(Vec<StructFieldInit>, IRBlockId), ()> {
    let mut current = block;
    let mut values_by_name: BTreeMap<String, ValueId> = BTreeMap::new();
    for field in fields {
        let (value, next) = lower_expr(&field.value, ctx, current, registry, diagnostics)?;
        values_by_name.insert(field.name.clone(), value);
        current = next;
    }

    let mut field_inits = Vec::with_capacity(declared.len());
    for (index, decl_field) in declared.iter().enumerate() {
        let value = values_by_name.remove(&decl_field.name).unwrap_or_else(|| {
            panic!(
                "alpha IR lower: named-field construction missing field `{}` after typecheck \
                 seal — resolve invariant violation",
                decl_field.name,
            )
        });
        field_inits.push(StructFieldInit {
            index: index as u32,
            value,
        });
    }
    Ok((field_inits, current))
}

/// Lower an `expr.field` access. The receiver's `ResolvedType` keys
/// the registry lookup; the field's `ResolvedType` (already on
/// `expr.resolution`) sizes the result.
pub(super) fn lower_field_access(
    receiver: &Expr,
    field: &str,
    field_resolution: &ResolvedType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(ValueId, IRBlockId), ()> {
    let (base, current) = lower_expr(receiver, ctx, block, registry, diagnostics)?;
    let entry = struct_entry_from_resolution(&receiver.resolution, registry, "field access");
    let definition =
        struct_definition_from_resolution(&receiver.resolution, registry, "field access");
    let (field_index, _) = definition.lookup_field(field).unwrap_or_else(|| {
        panic!(
            "alpha IR lower: field access missing field `{field}` after typecheck seal — \
             resolve invariant violation",
        )
    });
    let field_type = resolved_type_to_ir_type(field_resolution, registry);
    let struct_symbol = IRSymbol::from_identifier(&entry.identifier);
    let dest = ctx.fresh_value(field_type.clone());
    ctx.cfg.append(
        current,
        IRInstruction::FieldGet {
            base,
            dest,
            field_index,
            field_type,
            struct_symbol,
        },
    );
    Ok((dest, current))
}

fn struct_entry_from_resolution<'a>(
    resolution: &ResolvedType,
    registry: &'a GlobalRegistry,
    role: &str,
) -> &'a RegistryEntry {
    let Resolution::Global(id) = resolution.resolution else {
        panic!("alpha IR lower: struct {role} has Unresolved resolution after typecheck seal",);
    };
    registry.get(id).unwrap_or_else(|| {
        panic!("alpha IR lower: struct id {id} missing from registry — seal invariant violation",)
    })
}

fn struct_definition_from_resolution<'a>(
    resolution: &ResolvedType,
    registry: &'a GlobalRegistry,
    role: &str,
) -> &'a StructDefinition {
    let entry = struct_entry_from_resolution(resolution, registry, role);
    let GlobalKind::Struct(Some(definition)) = &entry.kind else {
        panic!(
            "alpha IR lower: struct {role} target `{}` has no lifted definition — \
             lift_signatures invariant violation",
            entry.identifier,
        );
    };
    definition
}

/// Diagnose every feature gap on a struct decl. Returns `true` when
/// any diagnostic was pushed; the caller drops the decl from the
/// package fragment in that case.
fn has_feature_gap(decl: &StructDecl, diagnostics: &mut Vec<Diagnostic>) -> bool {
    let mut gapped = false;
    if !decl.type_params.is_empty() {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha IR does not yet lower generic structs (`{}` has type parameters)",
                decl.name,
            ),
            decl.span,
        ));
        gapped = true;
    }
    for annotation in &decl.annotations {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha IR does not yet lower annotations on struct items (`@{}` on `{}`)",
                annotation.name, decl.name,
            ),
            annotation.span,
        ));
        gapped = true;
    }
    for field in &decl.fields {
        if field_has_feature_gap(&decl.name, field, diagnostics) {
            gapped = true;
        }
    }
    gapped
}

fn field_has_feature_gap(
    struct_name: &str,
    field: &StructField,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    if field.default.is_some() {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha IR does not yet lower default field values (on `{struct_name}.{}`)",
                field.name,
            ),
            field.span,
        ));
        return true;
    }
    false
}
