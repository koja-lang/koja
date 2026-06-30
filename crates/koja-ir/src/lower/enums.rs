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

use koja_ast::ast::{
    AnnotationKind, Diagnostic, EnumConstructionData, EnumDecl, EnumVariant, Expr, FieldInit,
};
use koja_ast::identifier::{Identifier, Resolution, ResolvedType};
use koja_typecheck::{
    EnumDefinition, GlobalKind, GlobalRegistry, RegistryEntry, ResolvedVariantData,
};

use crate::enum_decl::{
    EnumPayloadInit, IREnumDecl, IREnumVariant, IRVariantPayload, IRVariantTag,
};
use crate::function::{IRBlockId, IRInstruction, IRSymbol};
use crate::generics::Instantiation;
use crate::struct_decl::IRStructField;
use crate::types::{IRType, ValueId};

use super::ctx::{FnLowerCtx, LowerOutput};
use super::expr::lower_expr;
use super::ownership::materialize_owned;
use super::package::resolved_type_to_ir_type;
use super::structs::canonicalize_struct_inits;

/// The variant being constructed, bundled so the per-shape helpers
/// take one identity arg instead of two. Mirrors the `&RegistryEntry`
/// pattern in [`super::expr::emit_call`] (which threads a single
/// identity object through the bare- vs instance-call dispatch);
/// keeps `lower_struct_variant` under the clippy arg-count
/// threshold without bundling the ambient
/// `(ctx, block, registry, output)` tuple every other lower
/// helper threads explicitly. Private to this module — the IR
/// vocabulary keeps `tag` and `ty` as separate fields on
/// [`IRInstruction::EnumConstruct`], and `VariantTarget` is purely
/// a lowering-pipeline grouping.
struct VariantTarget {
    symbol: IRSymbol,
    tag: IRVariantTag,
}

/// Lower an `Item::Enum` against the typecheck registry. Returns
/// `None` for generic decls (they specialize through
/// [`crate::generics::instantiate`] off the typecheck registry) and
/// for decls where any feature-gap diagnostic surfaced. Tag
/// bounds-check (variant count > 256) surfaces as a feature-gap
/// diagnostic — the LLVM `i8` tag width caps the variant count, and
/// widening is a follow-up beyond this slice.
pub(super) fn lower_enum_decl(
    decl: &EnumDecl,
    package: &str,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Option<IREnumDecl> {
    if has_feature_gap(decl, &mut output.diagnostics) {
        return None;
    }
    let identifier = Identifier::new(package, decl.path.clone());
    let entry = registry.lookup(&identifier).map(|(_, entry)| entry)?;
    let GlobalKind::Enum(Some(definition)) = &entry.kind else {
        panic!(
            "IR lower: enum `{identifier}` has no lifted definition — \
             lift_signatures invariant violation",
        );
    };
    if definition.variants.len() > usize::from(u8::MAX) + 1 {
        output.diagnostics.push(Diagnostic::error(
            format!(
                "IR does not yet lower enums with more than {} variants \
                 (`{}` declares {})",
                u8::MAX as usize + 1,
                identifier,
                definition.variants.len(),
            ),
            decl.span,
        ));
        return None;
    }
    if !entry.type_params.is_empty() {
        return None;
    }
    let symbol = IRSymbol::from_identifier(&entry.identifier);
    let variants = lower_variants(definition, registry, &mut output.instantiations);
    Some(IREnumDecl { symbol, variants })
}

fn lower_variants(
    definition: &EnumDefinition,
    registry: &GlobalRegistry,
    instantiations: &mut Vec<Instantiation>,
) -> Vec<IREnumVariant> {
    let mut variants = Vec::with_capacity(definition.variants.len());
    for (index, variant) in definition.variants.iter().enumerate() {
        let payload = match &variant.data {
            ResolvedVariantData::Struct(fields) => {
                let mut ir_fields = Vec::with_capacity(fields.len());
                for (idx, field) in fields.iter().enumerate() {
                    ir_fields.push(IRStructField {
                        index: idx as u32,
                        ir_type: resolved_type_to_ir_type(&field.ty, registry, instantiations),
                        name: field.name.clone(),
                    });
                }
                IRVariantPayload::Struct(ir_fields)
            }
            ResolvedVariantData::Tuple(types) => {
                let translated = types
                    .iter()
                    .map(|ty| resolved_type_to_ir_type(ty, registry, instantiations))
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
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let entry = enum_entry_from_resolution(expr_resolution, registry);
    let definition = enum_definition_from_entry(entry);
    let symbol = resolved_enum_symbol(expr_resolution, registry, &mut output.instantiations);
    let (variant_index, variant) = definition.lookup_variant(variant_name).unwrap_or_else(|| {
        panic!(
            "IR lower: enum `{}` has no variant `{variant_name}` — \
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
            lower_tuple_variant(target, exprs, ctx, block, registry, output)
        }
        (ResolvedVariantData::Struct(declared), EnumConstructionData::Struct(fields)) => {
            lower_struct_variant(target, declared, fields, ctx, block, registry, output)
        }
        (declared, supplied) => panic!(
            "IR lower: enum `{}.{variant_name}` payload shape mismatch \
             (declared {declared:?}, supplied {supplied:?}) — typecheck seal violation",
            entry.identifier,
        ),
    }
}

/// The mangled [`IRSymbol`] for the enum named by `resolution`.
/// Mirrors [`super::structs`]'s `resolved_struct_symbol` — routes
/// through [`resolved_type_to_ir_type`] so any non-empty `type_args`
/// land in `instantiations`.
pub(super) fn resolved_enum_symbol(
    resolution: &ResolvedType,
    registry: &GlobalRegistry,
    instantiations: &mut Vec<Instantiation>,
) -> IRSymbol {
    match resolved_type_to_ir_type(resolution, registry, instantiations) {
        IRType::Enum(symbol) => symbol,
        other => panic!(
            "IR lower: enum construction target lowered to `{other:?}`, expected \
             IRType::Enum — typecheck seal must have caught this",
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
    ctx.mark_owned(dest);
    (dest, block)
}

fn lower_tuple_variant(
    target: VariantTarget,
    exprs: &[Expr],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let VariantTarget { symbol, tag } = target;
    let mut current = block;
    let mut values = Vec::with_capacity(exprs.len());
    for expr in exprs {
        let (value, next) = lower_expr(expr, ctx, current, registry, output)?;
        // Value semantics: an enum payload-store acquires an independent
        // value, so a borrowed heap-leaf source is cloned (rc-bumped) in.
        // The variant then owns a reference outliving the source local's
        // scope-exit drop.
        let payload_ty = ctx.type_of(value);
        let owned = materialize_owned(ctx, current, value, &payload_ty);
        values.push(owned);
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
    ctx.mark_owned(dest);
    Ok((dest, current))
}

fn lower_struct_variant(
    target: VariantTarget,
    declared: &[koja_typecheck::ResolvedStructField],
    fields: &[FieldInit],
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let VariantTarget { symbol, tag } = target;
    let (canonical, current) =
        canonicalize_struct_inits(declared, fields, ctx, block, registry, output)?;
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
    ctx.mark_owned(dest);
    Ok((dest, current))
}

pub(super) fn enum_entry_from_resolution<'a>(
    resolution: &ResolvedType,
    registry: &'a GlobalRegistry,
) -> &'a RegistryEntry {
    let ResolvedType::Named {
        resolution: Resolution::Global(id),
        ..
    } = resolution
    else {
        panic!("IR lower: enum construction has Unresolved resolution after typecheck seal",);
    };
    registry.get(*id).unwrap_or_else(|| {
        panic!("IR lower: enum id {id} missing from registry — seal invariant violation",)
    })
}

pub(super) fn enum_definition_from_entry(entry: &RegistryEntry) -> &EnumDefinition {
    let GlobalKind::Enum(Some(definition)) = &entry.kind else {
        panic!(
            "IR lower: enum construction target `{}` has no lifted definition — \
             lift_signatures invariant violation",
            entry.identifier,
        );
    };
    definition
}

fn has_feature_gap(decl: &EnumDecl, diagnostics: &mut Vec<Diagnostic>) -> bool {
    let mut gapped = false;
    for annotation in &decl.annotations {
        if matches!(annotation.kind(), AnnotationKind::Doc(_)) {
            continue;
        }
        diagnostics.push(Diagnostic::error(
            format!(
                "IR does not yet lower annotations on enum items (`@{}` on `{}`)",
                annotation.name,
                decl.name(),
            ),
            annotation.span,
        ));
        gapped = true;
    }
    for variant in &decl.variants {
        if variant_has_feature_gap(decl.name(), variant, diagnostics) {
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
    use koja_ast::ast::EnumVariantData;
    match &variant.data {
        EnumVariantData::Struct(fields) => {
            let mut gapped = false;
            for field in fields {
                if field.default.is_some() {
                    diagnostics.push(Diagnostic::error(
                        format!(
                            "IR does not yet lower default field values on struct \
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
