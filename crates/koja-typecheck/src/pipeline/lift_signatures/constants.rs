//! Constant lifting resolves the optional `: Type` annotation on
//! `const NAME[: Type] = expr`, validates the RHS shape, stamps every
//! `Expr.resolution` slot in the value subtree, and registers the
//! [`crate::registry::ConstantDefinition`] on the constant entry.
//!
//! The constant value surface is intentionally narrow: literals,
//! negated numerics, unit enum variants, structs of literals, and
//! all-literal binary literals.
//! Resolve never visits these expressions (the walker explicitly
//! skips `Item::Constant`). Lift owns the entire resolution. That
//! keeps the constant slice self-contained and lets seal verify
//! `Constant(Some(_))` without re-walking the AST.

use koja_ast::ast::{
    BinarySegment, Constant, Diagnostic, EnumConstructionData, Expr, ExprKind, FieldInit, Literal,
    StringPart, UnaryOp,
};
use koja_ast::identifier::{Identifier, Resolution, ResolvedType};
use koja_ast::span::Span;

use crate::pipeline::aliases::rewrite_through_aliases;
use crate::pipeline::resolve::coercion::{
    Mismatch, check_compatible_stamping, check_float_literal_finite,
};
use crate::pipeline::resolve::literals::{SegmentKind, resolve_segment};
use crate::registry::{
    ConstantDefinition, GlobalKind, GlobalRegistry, ResolvedStructField, ResolvedVariantData,
};

use super::LiftScope;
use super::types::{ResolutionScope, TypeParamScope, render_resolved, resolve_type_expr};

pub(super) fn lift_constant(
    constant: &mut Constant,
    scope: &mut LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(scope.package, vec![constant.name.clone()]);
    let Some((id, entry)) = scope.registry.lookup(&identifier) else {
        panic!(
            "lift_signatures: constant `{identifier}` missing from registry: \
             collect invariant violation",
        );
    };
    if matches!(entry.kind, GlobalKind::Constant(Some(_))) {
        return;
    }

    let type_params = TypeParamScope::new(&[]);
    let annotated = constant.type_annotation.as_ref().map(|type_expr| {
        resolve_type_expr(
            type_expr,
            type_params,
            scope.resolution_scope(),
            diagnostics,
        )
    });

    let value_scope = scope.resolution_scope();
    let inferred = resolve_constant_value(
        &mut constant.value,
        annotated.as_ref(),
        value_scope,
        diagnostics,
    );

    // Pin the constant's stamped type at the annotation when the
    // RHS is a coerced literal. `inferred` is still the literal's
    // default `Int` / `Float` head, but the coercion table now
    // carries the literal at the narrower target width and the
    // registry should reflect the visible type. When no annotation
    // exists, the inferred head is the visible type.
    let ty = annotated.unwrap_or(inferred);
    scope.registry.set_constant_definition(
        id,
        ConstantDefinition {
            ty,
            value: constant.value.clone(),
        },
    );
}

/// Walk the RHS, validate it's an allowed constant shape, stamp each
/// node's `resolution`, and yield the inferred type. `expected` is
/// the resolved annotation (if any), propagated to children for
/// per-field type checking. When the inferred head and `expected`
/// disagree, the literal-coercion path is consulted before falling
/// through to a strict mismatch diagnostic.
///
/// `scope` is the read-only [`ResolutionScope`] for the file the
/// constant is declared in (alias slice + current package +
/// registry). The constant value walk never mutates the registry:
/// definition stamping happens once at the [`lift_constant`] entry
/// point after this returns, so `&` is the right shape here.
fn resolve_constant_value(
    expr: &mut Expr,
    expected: Option<&ResolvedType>,
    scope: ResolutionScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let ty = match &mut expr.kind {
        ExprKind::Literal { value } => {
            check_float_literal_finite(value, expr.span, diagnostics);
            scope.registry.literal_type(value)
        }
        ExprKind::String { parts, .. } => {
            string_literal_type(parts, expr.span, scope.registry, diagnostics)
        }
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => negated_numeric_type(operand, scope, diagnostics),
        ExprKind::EnumConstruction {
            type_path,
            variant,
            data,
        } => enum_variant_type(type_path, variant, data, expr.span, scope, diagnostics),
        ExprKind::StructConstruction { type_path, fields } => {
            struct_construction_type(type_path, fields, expr.span, scope, diagnostics)
        }
        ExprKind::BinaryLiteral { segments } => binary_literal_type(segments, scope, diagnostics),
        ExprKind::Group { expr: inner } => {
            resolve_constant_value(inner, expected, scope, diagnostics)
        }
        _ => {
            diagnostics.push(Diagnostic::error(
                "constant values are limited to literals, negated numerics, unit enum \
                 variants, structs of literals, and binary literals",
                expr.span,
            ));
            ResolvedType::unresolved()
        }
    };

    if let Some(expected) = expected
        && ty.is_resolved()
        && expected.is_resolved()
    {
        match check_compatible_stamping(expr, &ty, expected, scope.registry) {
            None => {}
            Some(Mismatch::OutOfRange {
                rendered_value,
                width,
            }) => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "constant value `{rendered_value}` does not fit in `{}` \
                         (range {})",
                        width.label(),
                        width.range_label(),
                    ),
                    expr.span,
                ));
            }
            Some(Mismatch::Incompatible) => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "constant value type `{}` does not match annotation `{}`",
                        render_type(&ty, scope.registry),
                        render_type(expected, scope.registry),
                    ),
                    expr.span,
                ));
            }
        }
    }

    expr.resolution = ty.clone();
    ty
}

/// Validate a `<<...>>` constant RHS. Every segment value must be a
/// direct literal so the IR layer can fold the whole literal into
/// bytes at compile time. Width and fit rules are shared with the
/// resolve phase through [`resolve_segment`]. Yields `Binary` for a
/// byte-aligned total and `Bits` otherwise.
fn binary_literal_type(
    segments: &mut [BinarySegment],
    scope: ResolutionScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let mut total_bits: u64 = 0;
    let mut all_resolved = true;
    for segment in segments.iter_mut() {
        if !stamp_constant_segment_value(&mut segment.value, scope, diagnostics) {
            all_resolved = false;
            continue;
        }
        let Some(info) = resolve_segment(segment, scope.registry, diagnostics) else {
            all_resolved = false;
            continue;
        };
        if !segment_kind_matches_literal(&info.kind, &segment.value) {
            diagnostics.push(Diagnostic::error(
                "binary segment value does not match the segment's declared shape \
                 (integer segments take int literals, float segments take float literals)",
                segment.value.span,
            ));
            all_resolved = false;
            continue;
        }
        // Every constant segment is a literal, so its width is
        // static. A dynamic-width splice cannot reach here because
        // `stamp_constant_segment_value` already rejected the value.
        let Some(width_bits) = info.width_bits else {
            all_resolved = false;
            continue;
        };
        total_bits += width_bits;
    }
    if !all_resolved {
        return ResolvedType::unresolved();
    }
    let primitive_name = if total_bits.is_multiple_of(8) {
        "Binary"
    } else {
        "Bits"
    };
    scope.registry.primitive(primitive_name)
}

/// Stamp a constant binary segment's value with its literal type.
/// Only direct literals are allowed, since the value must fold at
/// compile time.
fn stamp_constant_segment_value(
    value: &mut Expr,
    scope: ResolutionScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    match &value.kind {
        ExprKind::Literal { value: literal } => {
            check_float_literal_finite(literal, value.span, diagnostics);
            value.resolution = scope.registry.literal_type(literal);
            true
        }
        ExprKind::String { parts, .. } => {
            let ty = string_literal_type(parts, value.span, scope.registry, diagnostics);
            if !ty.is_resolved() {
                return false;
            }
            value.resolution = ty;
            true
        }
        _ => {
            diagnostics.push(Diagnostic::error(
                "binary segment values in a constant must be literals",
                value.span,
            ));
            false
        }
    }
}

/// True when the segment's classified kind agrees with the literal
/// the value holds. [`resolve_segment`] does not cross-check the
/// value type for `: Type`-annotated segments, so the constant path
/// pins it here (a float segment folds a float literal's bits and
/// nothing else).
fn segment_kind_matches_literal(kind: &SegmentKind, value: &Expr) -> bool {
    match kind {
        SegmentKind::Integer => matches!(
            &value.kind,
            ExprKind::Literal {
                value: Literal::Int(_)
            }
        ),
        SegmentKind::Float => matches!(
            &value.kind,
            ExprKind::Literal {
                value: Literal::Float(_)
            }
        ),
        SegmentKind::Splice => false,
        SegmentKind::String => true,
    }
}

fn string_literal_type(
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
            "interpolated strings are not constant-evaluable",
            span,
        ));
        return ResolvedType::unresolved();
    }
    registry.primitive("String")
}

fn negated_numeric_type(
    operand: &mut Expr,
    scope: ResolutionScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let ty = resolve_constant_value(operand, None, scope, diagnostics);
    if !ty.is_resolved() {
        return ResolvedType::unresolved();
    }
    let int = scope.registry.primitive("Int");
    let float = scope.registry.primitive("Float");
    if ty == int || ty == float {
        ty
    } else {
        diagnostics.push(Diagnostic::error(
            "unary `-` requires a numeric literal",
            operand.span,
        ));
        ResolvedType::unresolved()
    }
}

fn enum_variant_type(
    type_path: &[String],
    variant: &str,
    data: &mut EnumConstructionData,
    span: Span,
    scope: ResolutionScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let Some(name) = type_path.last().map(String::as_str) else {
        diagnostics.push(Diagnostic::error("missing enum name", span));
        return ResolvedType::unresolved();
    };
    let identifier = lookup_constant_type_identifier(type_path, name, scope);
    let Some((enum_id, entry)) = scope.registry.lookup(&identifier) else {
        diagnostics.push(Diagnostic::error(format!("unknown enum `{name}`"), span));
        return ResolvedType::unresolved();
    };
    let GlobalKind::Enum(Some(def)) = &entry.kind else {
        diagnostics.push(Diagnostic::error(format!("`{name}` is not an enum"), span));
        return ResolvedType::unresolved();
    };
    let Some((_, resolved)) = def.lookup_variant(variant) else {
        diagnostics.push(Diagnostic::error(
            format!("enum `{name}` has no variant `{variant}`"),
            span,
        ));
        return ResolvedType::unresolved();
    };
    if !matches!(resolved.data, ResolvedVariantData::Unit) {
        diagnostics.push(Diagnostic::error(
            format!(
                "constant enum values must reference a unit variant, but `{name}.{variant}` \
                 carries a payload",
            ),
            span,
        ));
        return ResolvedType::unresolved();
    }
    if !matches!(data, EnumConstructionData::Unit) {
        diagnostics.push(Diagnostic::error(
            format!("`{name}.{variant}` is a unit variant and takes no arguments"),
            span,
        ));
        return ResolvedType::unresolved();
    }
    let _ = resolved;
    ResolvedType::leaf(Resolution::Global(enum_id))
}

fn struct_construction_type(
    type_path: &[String],
    fields: &mut [FieldInit],
    span: Span,
    scope: ResolutionScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let Some(name) = type_path.last().map(String::as_str) else {
        diagnostics.push(Diagnostic::error("missing struct name", span));
        return ResolvedType::unresolved();
    };
    let identifier = lookup_constant_type_identifier(type_path, name, scope);
    let Some((struct_id, entry)) = scope.registry.lookup(&identifier) else {
        diagnostics.push(Diagnostic::error(format!("unknown struct `{name}`"), span));
        return ResolvedType::unresolved();
    };
    let GlobalKind::Struct(Some(def)) = &entry.kind else {
        diagnostics.push(Diagnostic::error(format!("`{name}` is not a struct"), span));
        return ResolvedType::unresolved();
    };
    if !entry.type_params.is_empty() {
        diagnostics.push(Diagnostic::error(
            format!(
                "constant struct values do not yet support generic structs (`{name}` is \
                 generic)",
            ),
            span,
        ));
        return ResolvedType::unresolved();
    }
    let resolved_fields: Vec<ResolvedStructField> = def.fields.clone();
    if !validate_struct_fields(&resolved_fields, fields, name, span, diagnostics) {
        return ResolvedType::unresolved();
    }
    for field_init in fields.iter_mut() {
        let expected = resolved_fields
            .iter()
            .find(|f| f.name == field_init.name)
            .map(|f| f.ty.clone());
        resolve_constant_value(&mut field_init.value, expected.as_ref(), scope, diagnostics);
    }
    ResolvedType::leaf(Resolution::Global(struct_id))
}

fn validate_struct_fields(
    expected: &[ResolvedStructField],
    actual: &[FieldInit],
    struct_name: &str,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let mut ok = true;
    for field in expected {
        if !actual.iter().any(|f| f.name == field.name) {
            diagnostics.push(Diagnostic::error(
                format!("constant `{struct_name}` is missing field `{}`", field.name,),
                span,
            ));
            ok = false;
        }
    }
    for init in actual {
        if !expected.iter().any(|f| f.name == init.name) {
            diagnostics.push(Diagnostic::error(
                format!("`{struct_name}` has no field `{}`", init.name),
                init.span,
            ));
            ok = false;
        }
    }
    ok
}

/// Project a constant value's `type_path` (the full dotted path
/// the user wrote on `Foo.Variant{...}` / `Foo{...}`) onto a
/// registered [`Identifier`] under the constant scope's lookup
/// rules: an alias-bound head wins, otherwise fall back to the
/// current package. Constant value resolution today only accepts
/// single-segment heads, so multi-segment alias targets simply
/// won't resolve until nested-type lifting lands (same fall-through
/// behavior as `resolve_named` in [`super::types`]).
fn lookup_constant_type_identifier(
    type_path: &[String],
    name: &str,
    scope: ResolutionScope<'_>,
) -> Identifier {
    if let Some(target) =
        rewrite_through_aliases(scope.aliases, type_path, scope.package, scope.registry)
    {
        return target;
    }
    Identifier::new(scope.package, vec![name.to_string()])
}

fn render_type(ty: &ResolvedType, registry: &GlobalRegistry) -> String {
    match ty {
        ResolvedType::Anonymous(_) | ResolvedType::Union(_) => render_resolved(ty, registry),
        ResolvedType::Named {
            resolution: Resolution::Global(id),
            ..
        } => registry
            .get(*id)
            .map(|e| e.identifier.qualified_name())
            .unwrap_or_else(|| format!("<id {id}>")),
        ResolvedType::Named {
            resolution: Resolution::Local(local_id),
            ..
        } => format!("<local {local_id}>"),
        ResolvedType::Named {
            resolution: Resolution::TypeParam { owner, index },
            ..
        } => registry
            .type_param_name(*owner, *index)
            .map(str::to_string)
            .unwrap_or_else(|| format!("<typeparam {owner}#{index}>")),
        ResolvedType::Named {
            resolution: Resolution::Unresolved,
            ..
        }
        | ResolvedType::Unresolved => "<unresolved>".to_string(),
    }
}
