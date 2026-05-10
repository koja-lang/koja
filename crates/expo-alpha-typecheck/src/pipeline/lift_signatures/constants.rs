//! Constant lifting: resolve the optional `: Type` annotation on
//! `const NAME[: Type] = expr`, validate the RHS shape, stamp every
//! `Expr.resolution` slot in the value subtree, and register the
//! [`crate::registry::ConstantDefinition`] on the constant entry.
//!
//! The constant value surface is intentionally narrow — literals,
//! negated numerics, unit enum variants, and structs of literals.
//! Resolve never visits these expressions (the walker explicitly
//! skips `Item::Constant`); lift owns the entire resolution. That
//! keeps the constant slice self-contained and lets seal verify
//! `Constant(Some(_))` without re-walking the AST.

use expo_ast::ast::{
    Constant, Diagnostic, EnumConstructionData, Expr, ExprKind, FieldInit, Literal, StringPart,
    UnaryOp,
};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

use crate::pipeline::resolve::coercion::{Coercions, Compatible, check_compatible, coercion_span};
use crate::registry::{
    ConstantDefinition, GlobalKind, GlobalRegistry, ResolvedStructField, ResolvedVariantData,
};

use super::types::{TypeParamScope, render_resolved, resolve_type_expr};

/// Constant-pass walk inputs. Bundles the read-only registry view
/// (constant initializers don't need the `&mut` registry surface
/// — that mutation happens at the [`lift_constant`] entry point
/// after the walk produces the resolved value), the package-scope
/// hint used for unqualified type lookups, and the program-wide
/// numeric-literal coercion sink shared with the resolve pass.
struct ConstCtx<'a> {
    coercions: &'a mut Coercions,
    package: &'a str,
    registry: &'a GlobalRegistry,
}

pub(super) fn lift_constant(
    constant: &mut Constant,
    package: &str,
    registry: &mut GlobalRegistry,
    coercions: &mut Coercions,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(package, vec![constant.name.clone()]);
    let Some((id, entry)) = registry.lookup(&identifier) else {
        panic!(
            "lift_signatures: constant `{identifier}` missing from registry — \
             collect invariant violation",
        );
    };
    if matches!(entry.kind, GlobalKind::Constant(Some(_))) {
        return;
    }

    let scope = TypeParamScope::new(&[]);
    let annotated = constant
        .type_annotation
        .as_ref()
        .map(|type_expr| resolve_type_expr(type_expr, scope, package, registry, diagnostics));

    let mut ctx = ConstCtx {
        coercions,
        package,
        registry,
    };
    let inferred = resolve_constant_value(
        &mut constant.value,
        annotated.as_ref(),
        &mut ctx,
        diagnostics,
    );

    // Pin the constant's stamped type at the annotation when the
    // RHS is a coerced literal: `inferred` is still the literal's
    // default `Int` / `Float` head, but the coercion table now
    // carries the literal at the narrower target width and the
    // registry should reflect the visible type. When no annotation
    // exists, the inferred head is the visible type.
    let ty = annotated.unwrap_or(inferred);
    registry.set_constant_definition(
        id,
        ConstantDefinition {
            ty,
            value: constant.value.clone(),
        },
    );
}

/// Walk the RHS, validate it's an allowed constant shape, stamp each
/// node's `resolution`, and yield the inferred type. `expected` is
/// the resolved annotation (if any) — propagated to children for
/// per-field type checking. When the inferred head and `expected`
/// disagree, the literal-coercion path is consulted before falling
/// through to a strict mismatch diagnostic.
fn resolve_constant_value(
    expr: &mut Expr,
    expected: Option<&ResolvedType>,
    ctx: &mut ConstCtx<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let ty = match &mut expr.kind {
        ExprKind::Literal { value } => literal_type(value, ctx.registry),
        ExprKind::String { parts, .. } => {
            string_literal_type(parts, expr.span, ctx.registry, diagnostics)
        }
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => negated_numeric_type(operand, ctx, diagnostics),
        ExprKind::EnumConstruction {
            type_path,
            variant,
            data,
        } => enum_variant_type(
            type_path,
            variant,
            data,
            expr.span,
            ctx.package,
            ctx.registry,
            diagnostics,
        ),
        ExprKind::StructConstruction { type_path, fields } => {
            struct_construction_type(type_path, fields, expr.span, ctx, diagnostics)
        }
        ExprKind::Group { expr: inner } => {
            resolve_constant_value(inner, expected, ctx, diagnostics)
        }
        _ => {
            diagnostics.push(Diagnostic::error(
                "constant values are limited to literals, negated numerics, unit enum \
                 variants, and structs of literals",
                expr.span,
            ));
            ResolvedType::unresolved()
        }
    };

    if let Some(expected) = expected
        && ty.is_resolved()
        && expected.is_resolved()
    {
        match check_compatible(expr, &ty, expected, ctx.registry) {
            Compatible::Strict => {}
            Compatible::Coerced(width) => {
                ctx.coercions.insert(coercion_span(expr), width);
            }
            Compatible::OutOfRange {
                rendered_value,
                width,
            } => {
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
            Compatible::Incompatible => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "constant value type `{}` does not match annotation `{}`",
                        render_type(&ty, ctx.registry),
                        render_type(expected, ctx.registry),
                    ),
                    expr.span,
                ));
            }
        }
    }

    expr.resolution = ty.clone();
    ty
}

fn literal_type(value: &Literal, registry: &GlobalRegistry) -> ResolvedType {
    match value {
        Literal::Bool(_) => registry.primitive("Bool"),
        Literal::Float(_) => registry.primitive("Float"),
        Literal::Int(_) => registry.primitive("Int"),
        Literal::String(_) => registry.primitive("String"),
        Literal::Unit => registry.primitive("Unit"),
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
    ctx: &mut ConstCtx<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let ty = resolve_constant_value(operand, None, ctx, diagnostics);
    if !ty.is_resolved() {
        return ResolvedType::unresolved();
    }
    let int = ctx.registry.primitive("Int");
    let float = ctx.registry.primitive("Float");
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
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let Some(name) = type_path.last().map(String::as_str) else {
        diagnostics.push(Diagnostic::error("missing enum name", span));
        return ResolvedType::unresolved();
    };
    let identifier = Identifier::new(package, vec![name.to_string()]);
    let Some((enum_id, entry)) = registry.lookup(&identifier) else {
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
                "constant enum values must reference a unit variant — `{name}.{variant}` \
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
    ctx: &mut ConstCtx<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let Some(name) = type_path.last().map(String::as_str) else {
        diagnostics.push(Diagnostic::error("missing struct name", span));
        return ResolvedType::unresolved();
    };
    let identifier = Identifier::new(ctx.package, vec![name.to_string()]);
    let Some((struct_id, entry)) = ctx.registry.lookup(&identifier) else {
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
        resolve_constant_value(&mut field_init.value, expected.as_ref(), ctx, diagnostics);
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

fn render_type(ty: &ResolvedType, registry: &GlobalRegistry) -> String {
    match ty {
        ResolvedType::Anonymous(_) => render_resolved(ty, registry),
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
