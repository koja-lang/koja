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

use crate::registry::{
    ConstantDefinition, GlobalKind, GlobalRegistry, ResolvedStructField, ResolvedVariantData,
};

use super::types::{TypeParamScope, resolve_type_expr};

pub(super) fn lift_constant(
    constant: &mut Constant,
    package: &str,
    registry: &mut GlobalRegistry,
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

    let inferred = resolve_constant_value(
        &mut constant.value,
        annotated.as_ref(),
        package,
        registry,
        diagnostics,
    );

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
/// per-field type checking.
fn resolve_constant_value(
    expr: &mut Expr,
    expected: Option<&ResolvedType>,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let ty = match &mut expr.kind {
        ExprKind::Literal { value } => literal_type(value, registry),
        ExprKind::String { parts, .. } => {
            string_literal_type(parts, expr.span, registry, diagnostics)
        }
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => negated_numeric_type(operand, package, registry, diagnostics),
        ExprKind::EnumConstruction {
            type_path,
            variant,
            data,
        } => enum_variant_type(
            type_path,
            variant,
            data,
            expr.span,
            package,
            registry,
            diagnostics,
        ),
        ExprKind::StructConstruction { type_path, fields } => {
            struct_construction_type(type_path, fields, expr.span, package, registry, diagnostics)
        }
        ExprKind::Group { expr: inner } => {
            resolve_constant_value(inner, expected, package, registry, diagnostics)
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
        && ty != *expected
    {
        diagnostics.push(Diagnostic::error(
            format!(
                "constant value type `{}` does not match annotation `{}`",
                render_type(&ty, registry),
                render_type(expected, registry),
            ),
            expr.span,
        ));
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
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let ty = resolve_constant_value(operand, None, package, registry, diagnostics);
    if !ty.is_resolved() {
        return ResolvedType::unresolved();
    }
    let int = registry.primitive("Int");
    let float = registry.primitive("Float");
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
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let Some(name) = type_path.last().map(String::as_str) else {
        diagnostics.push(Diagnostic::error("missing struct name", span));
        return ResolvedType::unresolved();
    };
    let identifier = Identifier::new(package, vec![name.to_string()]);
    let Some((struct_id, entry)) = registry.lookup(&identifier) else {
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
        resolve_constant_value(
            &mut field_init.value,
            expected.as_ref(),
            package,
            registry,
            diagnostics,
        );
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
    match ty.resolution {
        Resolution::Global(id) => registry
            .get(id)
            .map(|e| e.identifier.qualified_name())
            .unwrap_or_else(|| format!("<id {id}>")),
        Resolution::Local(local_id) => format!("<local {local_id}>"),
        Resolution::TypeParam { owner, index } => registry
            .type_param_name(owner, index)
            .map(str::to_string)
            .unwrap_or_else(|| format!("<typeparam {owner}#{index}>")),
        Resolution::Unresolved => "<unresolved>".to_string(),
    }
}
