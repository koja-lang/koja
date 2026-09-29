//! Default-value lifting for struct and enum struct-variant fields.
//!
//! Lift stores an *unresolved* clone of the default AST on the
//! registry field so each construction site that omits the field can
//! clone it and re-resolve it against the substituted field type
//! (that per-site resolution is what makes `Option.None` and `[]`
//! work on generic fields). Only the syntactic shape is validated
//! here. The resolve walker trial-resolves every default in the
//! declaring file's scope once all definitions are stamped, so name
//! and type errors surface at the declaration.
//!
//! The stored clone is marked synthetic here, in the same walk that
//! checks its shape, so a site clones it without another walk and
//! LSP position lookups in the declaring file skip every synthesized
//! node. [`check_default_shape`] is the one description of the
//! default grammar: an arm that accepts a shape also marks it.

use koja_ast::ast::{
    Diagnostic, EnumConstructionData, Expr, ExprKind, FieldInit, Name, StringPart, StructField,
    UnaryOp,
};

use crate::pipeline::resolve::static_dotted_path;

/// Validate the shape of `field`'s default (if any) and yield a
/// synthetic-spanned, unresolved clone for registry storage.
/// Shape-invalid defaults diagnose and store as `None`, so
/// downstream sites fall back to the ordinary missing-field
/// diagnostic.
pub(super) fn lift_field_default(
    field: &StructField,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Box<Expr>> {
    let default = field.default.as_ref()?;
    let mut stored = default.clone();
    if !check_default_shape(&mut stored, diagnostics) {
        return None;
    }
    Some(Box::new(stored))
}

/// Recursive check over the allowed default-value shapes that marks
/// every span it accepts synthetic. Every shape is side-effect-free
/// and re-resolvable in any package, so a site-time re-resolution
/// can never observably diverge from the declaration.
///
/// Diagnostics use the span as it was before marking, so they point
/// at the declaration like any other error.
fn check_default_shape(expr: &mut Expr, diagnostics: &mut Vec<Diagnostic>) -> bool {
    let span = expr.span;
    expr.span = span.as_synthetic();
    match &mut expr.kind {
        ExprKind::BinaryLiteral { segments } => {
            let mut ok = true;
            for segment in segments {
                segment.span = segment.span.as_synthetic();
                if matches!(
                    &segment.value.kind,
                    ExprKind::Literal { .. } | ExprKind::String { .. }
                ) {
                    ok &= check_default_shape(&mut segment.value, diagnostics);
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
        // A unit variant or a constant read (`Duration.ZERO` and
        // `Color.Red` parse to the same node), a payload variant, or
        // a dotted struct literal such as `Pkg.Type{...}`, which
        // parses as a struct-shaped variant. Resolve tells them apart,
        // rewrites the struct literal to a struct construction, and
        // rejects anything else that lands here.
        ExprKind::EnumConstruction {
            type_path,
            variant,
            data,
        } => {
            mark_names_synthetic(type_path);
            variant.span = variant.span.as_synthetic();
            match data {
                EnumConstructionData::Struct(fields) => check_field_inits(fields, diagnostics),
                EnumConstructionData::Tuple(elements) => check_all(elements, diagnostics),
                EnumConstructionData::Unit => true,
            }
        }
        // A constant read in its bare (`MAX`) or lowercase-leaf
        // (`Pkg.limit`) spelling. Resolve rejects a name that is not
        // a constant, and resolves in the declaring package with no
        // locals, so the value is the same at every site.
        ExprKind::Ident { .. } => true,
        kind @ ExprKind::FieldAccess { .. } if static_dotted_path(kind).is_some() => {
            mark_path_synthetic(kind);
            true
        }
        ExprKind::Group { expr: inner } => check_default_shape(inner, diagnostics),
        ExprKind::List { elements } => check_all(elements, diagnostics),
        ExprKind::Literal { .. } => true,
        ExprKind::Map { entries } => entries.iter_mut().fold(true, |ok, (key, value)| {
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
                    span,
                ));
            }
            !interpolated
        }
        ExprKind::StructConstruction { type_path, fields } => {
            mark_names_synthetic(type_path);
            check_field_inits(fields, diagnostics)
        }
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => check_default_shape(operand, diagnostics),
        _ => {
            diagnostics.push(Diagnostic::error(
                "default field values are limited to literals, negated numerics, enum \
                 variants, constants, binary literals, and struct, list, map, or set literals \
                 of those",
                span,
            ));
            false
        }
    }
}

/// Check every element, reporting each failure rather than stopping
/// at the first.
fn check_all(elements: &mut [Expr], diagnostics: &mut Vec<Diagnostic>) -> bool {
    elements.iter_mut().fold(true, |ok, element| {
        check_default_shape(element, diagnostics) && ok
    })
}

/// Check every field value and mark each init's own span and name
/// synthetic.
fn check_field_inits(fields: &mut [FieldInit], diagnostics: &mut Vec<Diagnostic>) -> bool {
    fields.iter_mut().fold(true, |ok, field| {
        field.span = field.span.as_synthetic();
        field.name.span = field.name.span.as_synthetic();
        check_default_shape(&mut field.value, diagnostics) && ok
    })
}

fn mark_names_synthetic(names: &mut [Name]) {
    for name in names {
        name.span = name.span.as_synthetic();
    }
}

/// Mark a static dotted path (`Pkg.limit`) synthetic below its root:
/// each field name and each receiver down the chain. The chain is
/// `Ident` and `FieldAccess` nodes only, which [`static_dotted_path`]
/// has already confirmed.
fn mark_path_synthetic(kind: &mut ExprKind) {
    if let ExprKind::FieldAccess { receiver, field } = kind {
        field.span = field.span.as_synthetic();
        receiver.span = receiver.span.as_synthetic();
        mark_path_synthetic(&mut receiver.kind);
    }
}
