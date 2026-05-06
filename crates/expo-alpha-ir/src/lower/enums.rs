//! Enum-shaped lowering: declarations and enum-variant construction.
//! Mirrors [`super::structs`] — one helper per AST shape.
//!
//! Decl lowering pulls the canonical variant roster off the
//! typecheck registry's [`GlobalKind::Enum(Some(definition))`] so we
//! never re-resolve a `TypeExpr` here. Construction does the same:
//! typecheck has already validated names and types, so IR's job is
//! purely "stamp positional indices onto the variant tag and the
//! resolved per-payload-field [`IRType`] / [`ValueId`] onto the
//! [`IRInstruction::EnumConstruct`]".
//!
//! Per-shape construction is split into three small helpers
//! (`lower_unit_variant` / `lower_tuple_variant` / `lower_struct_variant`)
//! so each arm stays under the function-size guideline. The dispatch
//! is the surface entry — it picks the variant by name off the
//! registry, then hands off to the per-shape helper based on the
//! variant's declared payload.

use expo_alpha_typecheck::{
    EnumDefinition, GlobalKind, GlobalRegistry, RegistryEntry, ResolvedVariantData,
};
use expo_ast::ast::{Diagnostic, EnumConstructionData, EnumDecl, EnumVariant, Expr, FieldInit};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};

use crate::enum_decl::{
    EnumPayloadInit, IREnumDecl, IREnumVariant, IRVariantPayload, IRVariantTag,
};
use crate::function::{IRBlockId, IRInstruction, IRSymbol};
use crate::struct_decl::IRStructField;
use crate::types::{IRType, ValueId};

use super::ctx::FnLowerCtx;
use super::expr::lower_expr;
use super::package::resolved_type_to_ir_type;
use super::structs::canonicalize_struct_inits;

/// The variant being constructed, bundled so the per-shape helpers
/// take one identity arg instead of two. Mirrors the `&RegistryEntry`
/// pattern in [`super::expr::emit_call`] (which threads a single
/// identity object through the bare- vs instance-call dispatch);
/// keeps `lower_struct_variant` under the clippy arg-count
/// threshold without bundling the ambient
/// `(ctx, block, registry, diagnostics)` tuple every other lower
/// helper threads explicitly. Private to this module — the IR
/// vocabulary keeps `tag` and `ty` as separate fields on
/// [`IRInstruction::EnumConstruct`], and `VariantTarget` is purely
/// a lowering-pipeline grouping.
struct VariantTarget {
    symbol: IRSymbol,
    tag: IRVariantTag,
}

/// Lower an `Item::Enum` against the typecheck registry. Returns
/// `None` if any feature-gap diagnostic surfaced (matches the
/// per-decl fail-fast contract: the offending decl is dropped from
/// the package). Tag bounds-check (variant count > 256) surfaces as
/// a feature-gap diagnostic — the LLVM `i8` tag width caps the
/// variant count, and widening is a follow-up beyond this slice.
pub(super) fn lower_enum_decl(
    decl: &EnumDecl,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<IREnumDecl> {
    if has_feature_gap(decl, diagnostics) {
        return None;
    }
    let identifier = Identifier::new(package, vec![decl.name.clone()]);
    let entry = registry.lookup(&identifier).map(|(_, entry)| entry)?;
    let GlobalKind::Enum(Some(definition)) = &entry.kind else {
        panic!(
            "alpha IR lower: enum `{identifier}` has no lifted definition — \
             lift_signatures invariant violation",
        );
    };
    if definition.variants.len() > usize::from(u8::MAX) + 1 {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha IR does not yet lower enums with more than {} variants \
                 (`{}` declares {})",
                u8::MAX as usize + 1,
                identifier,
                definition.variants.len(),
            ),
            decl.span,
        ));
        return None;
    }
    let symbol = IRSymbol::from_identifier(&entry.identifier);
    let variants = lower_variants(definition, registry);
    Some(IREnumDecl { symbol, variants })
}

fn lower_variants(definition: &EnumDefinition, registry: &GlobalRegistry) -> Vec<IREnumVariant> {
    let mut variants = Vec::with_capacity(definition.variants.len());
    for (index, variant) in definition.variants.iter().enumerate() {
        let payload = match &variant.data {
            ResolvedVariantData::Struct(fields) => {
                let mut ir_fields = Vec::with_capacity(fields.len());
                for (idx, field) in fields.iter().enumerate() {
                    ir_fields.push(IRStructField {
                        index: idx as u32,
                        ir_type: resolved_type_to_ir_type(&field.ty, registry),
                        name: field.name.clone(),
                    });
                }
                IRVariantPayload::Struct(ir_fields)
            }
            ResolvedVariantData::Tuple(types) => {
                let translated = types
                    .iter()
                    .map(|ty| resolved_type_to_ir_type(ty, registry))
                    .collect();
                IRVariantPayload::Tuple(translated)
            }
            ResolvedVariantData::Unit => IRVariantPayload::Unit,
        };
        variants.push(IREnumVariant {
            name: variant.name.clone(),
            payload,
            tag: IRVariantTag(index as u8),
        });
    }
    variants
}

/// Lower an enum literal `Type.Variant(...)` / `Type.Variant{...}` /
/// `Type.Variant`. Picks the enum off the call-site
/// `expr.resolution`, finds the variant by name, and dispatches to
/// the per-shape helper. The shape match is guaranteed by typecheck
/// resolve; reaching a mismatch here is an invariant violation.
pub(super) fn lower_enum_construction(
    variant_name: &str,
    data: &EnumConstructionData,
    expr_resolution: &ResolvedType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(ValueId, IRBlockId), ()> {
    let entry = enum_entry_from_resolution(expr_resolution, registry);
    let definition = enum_definition_from_entry(entry);
    let symbol = IRSymbol::from_identifier(&entry.identifier);
    let (variant_index, variant) = definition.lookup_variant(variant_name).unwrap_or_else(|| {
        panic!(
            "alpha IR lower: enum `{}` has no variant `{variant_name}` — \
             typecheck seal must have rejected this",
            entry.identifier,
        )
    });
    let target = VariantTarget {
        symbol,
        tag: IRVariantTag(variant_index as u8),
    };
    match (&variant.data, data) {
        (ResolvedVariantData::Unit, EnumConstructionData::Unit) => {
            Ok(lower_unit_variant(target, ctx, block))
        }
        (ResolvedVariantData::Tuple(_), EnumConstructionData::Tuple(exprs)) => {
            lower_tuple_variant(target, exprs, ctx, block, registry, diagnostics)
        }
        (ResolvedVariantData::Struct(declared), EnumConstructionData::Struct(fields)) => {
            lower_struct_variant(target, declared, fields, ctx, block, registry, diagnostics)
        }
        (declared, supplied) => panic!(
            "alpha IR lower: enum `{}.{variant_name}` payload shape mismatch \
             (declared {declared:?}, supplied {supplied:?}) — typecheck seal violation",
            entry.identifier,
        ),
    }
}

fn lower_unit_variant(
    target: VariantTarget,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
) -> (ValueId, IRBlockId) {
    let VariantTarget { symbol, tag } = target;
    let dest = ctx.fresh_value(IRType::Enum(symbol.clone()));
    ctx.cfg.append(
        block,
        IRInstruction::EnumConstruct {
            dest,
            payload: EnumPayloadInit::Unit,
            tag,
            ty: symbol,
        },
    );
    (dest, block)
}

fn lower_tuple_variant(
    target: VariantTarget,
    exprs: &[Expr],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(ValueId, IRBlockId), ()> {
    let VariantTarget { symbol, tag } = target;
    let mut current = block;
    let mut values = Vec::with_capacity(exprs.len());
    for expr in exprs {
        let (value, next) = lower_expr(expr, ctx, current, registry, diagnostics)?;
        values.push(value);
        current = next;
    }
    let dest = ctx.fresh_value(IRType::Enum(symbol.clone()));
    ctx.cfg.append(
        current,
        IRInstruction::EnumConstruct {
            dest,
            payload: EnumPayloadInit::Tuple(values),
            tag,
            ty: symbol,
        },
    );
    Ok((dest, current))
}

fn lower_struct_variant(
    target: VariantTarget,
    declared: &[expo_alpha_typecheck::ResolvedStructField],
    fields: &[FieldInit],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(ValueId, IRBlockId), ()> {
    let VariantTarget { symbol, tag } = target;
    let (canonical, current) =
        canonicalize_struct_inits(declared, fields, ctx, block, registry, diagnostics)?;
    let dest = ctx.fresh_value(IRType::Enum(symbol.clone()));
    ctx.cfg.append(
        current,
        IRInstruction::EnumConstruct {
            dest,
            payload: EnumPayloadInit::Struct(canonical),
            tag,
            ty: symbol,
        },
    );
    Ok((dest, current))
}

fn enum_entry_from_resolution<'a>(
    resolution: &ResolvedType,
    registry: &'a GlobalRegistry,
) -> &'a RegistryEntry {
    let Resolution::Global(id) = resolution.resolution else {
        panic!("alpha IR lower: enum construction has Unresolved resolution after typecheck seal",);
    };
    registry.get(id).unwrap_or_else(|| {
        panic!("alpha IR lower: enum id {id} missing from registry — seal invariant violation",)
    })
}

fn enum_definition_from_entry(entry: &RegistryEntry) -> &EnumDefinition {
    let GlobalKind::Enum(Some(definition)) = &entry.kind else {
        panic!(
            "alpha IR lower: enum construction target `{}` has no lifted definition — \
             lift_signatures invariant violation",
            entry.identifier,
        );
    };
    definition
}

fn has_feature_gap(decl: &EnumDecl, diagnostics: &mut Vec<Diagnostic>) -> bool {
    let mut gapped = false;
    if !decl.type_params.is_empty() {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha IR does not yet lower generic enums (`{}` has type parameters)",
                decl.name,
            ),
            decl.span,
        ));
        gapped = true;
    }
    for annotation in &decl.annotations {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha IR does not yet lower annotations on enum items (`@{}` on `{}`)",
                annotation.name, decl.name,
            ),
            annotation.span,
        ));
        gapped = true;
    }
    for variant in &decl.variants {
        if variant_has_feature_gap(&decl.name, variant, diagnostics) {
            gapped = true;
        }
    }
    gapped
}

fn variant_has_feature_gap(
    enum_name: &str,
    variant: &EnumVariant,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    use expo_ast::ast::EnumVariantData;
    match &variant.data {
        EnumVariantData::Struct(fields) => {
            let mut gapped = false;
            for field in fields {
                if field.default.is_some() {
                    diagnostics.push(Diagnostic::error(
                        format!(
                            "alpha IR does not yet lower default field values on struct \
                             variants (on `{enum_name}.{}.{}`)",
                            variant.name, field.name,
                        ),
                        field.span,
                    ));
                    gapped = true;
                }
            }
            gapped
        }
        EnumVariantData::Tuple(_) | EnumVariantData::Unit => false,
    }
}
