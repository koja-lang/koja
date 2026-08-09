//! Default-value lifting for struct and enum struct-variant fields.
//!
//! Lift stores the *unresolved* default AST on the registry field so
//! each construction site that omits the field can clone it and
//! re-resolve it against the substituted field type (that per-site
//! resolution is what makes `Option.None` and `[]` work on generic
//! fields). Only the syntactic shape is validated here. The resolve
//! walker trial-resolves every stored default in the declaring
//! package's scope once all definitions are stamped, so name and
//! type errors surface at the declaration.

use koja_ast::ast::{
    Diagnostic, EnumConstructionData, Expr, ExprKind, StringPart, StructField, UnaryOp,
};

/// Validate the shape of `field`'s default (if any) and yield the
/// unresolved AST for registry storage. Shape-invalid defaults
/// diagnose and store as `None`, so downstream sites fall back to
/// the ordinary missing-field diagnostic.
pub(super) fn lift_field_default(
    field: &StructField,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Box<Expr>> {
    let default = field.default.as_ref()?;
    if !check_default_shape(default, diagnostics) {
        return None;
    }
    Some(Box::new(default.clone()))
}

/// Recursive check over the allowed default-value shapes: every
/// shape is side-effect-free and re-resolvable in any package, so a
/// site-time re-resolution can never observably diverge from the
/// declaration.
fn check_default_shape(expr: &Expr, diagnostics: &mut Vec<Diagnostic>) -> bool {
    match &expr.kind {
        ExprKind::BinaryLiteral { segments } => {
            let mut ok = true;
            for segment in segments {
                if matches!(
                    &segment.value.kind,
                    ExprKind::Literal { .. } | ExprKind::String { .. }
                ) {
                    ok &= check_default_shape(&segment.value, diagnostics);
                } else {
                    diagnostics.push(Diagnostic::error(
                        "binary segment values in a default field value must be literals",
                        segment.value.span,
                    ));
                    ok = false;
                }
            }
            ok
        }
        ExprKind::EnumConstruction {
            data: EnumConstructionData::Unit,
            ..
        } => true,
        ExprKind::Group { expr: inner } => check_default_shape(inner, diagnostics),
        ExprKind::List { elements } => elements.iter().fold(true, |ok, element| {
            check_default_shape(element, diagnostics) && ok
        }),
        ExprKind::Literal { .. } => true,
        ExprKind::Map { entries } => entries.iter().fold(true, |ok, (key, value)| {
            let key_ok = check_default_shape(key, diagnostics);
            let value_ok = check_default_shape(value, diagnostics);
            key_ok && value_ok && ok
        }),
        ExprKind::String { parts, .. } => {
            let interpolated = parts
                .iter()
                .any(|part| matches!(part, StringPart::Interpolation { .. }));
            if interpolated {
                diagnostics.push(Diagnostic::error(
                    "interpolated strings are not allowed in default field values",
                    expr.span,
                ));
            }
            !interpolated
        }
        ExprKind::StructConstruction { fields, .. } => fields.iter().fold(true, |ok, field| {
            check_default_shape(&field.value, diagnostics) && ok
        }),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => check_default_shape(operand, diagnostics),
        _ => {
            diagnostics.push(Diagnostic::error(
                "default field values are limited to literals, negated numerics, unit enum \
                 variants, binary literals, and struct, list, map, or set literals of those",
                expr.span,
            ));
            false
        }
    }
}
