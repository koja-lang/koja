//! The side-effect-free value grammar, shared by field defaults and
//! constants, plus default-value lifting for struct and enum
//! struct-variant fields.
//!
//! Lift validates only the syntactic shape of a default and stores an
//! unresolved clone on the registry field. A construction site that
//! omits the field clones it and resolves it against the substituted
//! field type, which is what makes `Option.None` and `[]` work on
//! generic fields. The resolve walker trial-resolves every default in
//! its declaring file once all definitions are stamped, so name and
//! type errors surface at the declaration.
//!
//! The walk that checks a default's shape also marks the clone
//! synthetic, so LSP position lookups skip the fill at every site and
//! no second walk has to know the grammar. A constant keeps its spans,
//! because its value is resolved once where it is written.
//! [`check_shape`] is the one description of the grammar, and
//! [`ShapeRules`] is the only difference between the two callers.

use koja_ast::ast::{
    Diagnostic, EnumConstructionData, Expr, ExprKind, FieldInit, Name, StringPart, StructField,
    UnaryOp,
};
use koja_ast::span::Span;

use crate::pipeline::resolve::static_dotted_path;

/// What a caller of [`check_shape`] does with the nodes it accepts.
struct ShapeRules {
    /// Mark every accepted span synthetic. Field defaults set this,
    /// because the stored clone is filled in at other sites.
    mark_synthetic: bool,
    /// Plural noun for diagnostics, such as "default field values".
    subject: &'static str,
}

impl ShapeRules {
    /// `span` as the accepted node should carry it.
    fn mark(&self, span: Span) -> Span {
        if self.mark_synthetic {
            span.as_synthetic()
        } else {
            span
        }
    }
}

const CONSTANT_RULES: ShapeRules = ShapeRules {
    mark_synthetic: false,
    subject: "constant values",
};

const DEFAULT_RULES: ShapeRules = ShapeRules {
    mark_synthetic: true,
    subject: "default field values",
};

/// Validate the shape of a constant's value. Spans stay as written.
pub(super) fn check_constant_shape(expr: &mut Expr, diagnostics: &mut Vec<Diagnostic>) -> bool {
    check_shape(expr, &CONSTANT_RULES, diagnostics)
}

/// Validate the shape of `field`'s default (if any) and yield the
/// clone for registry storage. A shape-invalid default diagnoses and
/// stores as `None`, so sites fall back to the ordinary missing-field
/// diagnostic.
pub(super) fn lift_field_default(
    field: &StructField,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Box<Expr>> {
    let default = field.default.as_ref()?;
    let mut stored = default.clone();
    if !check_shape(&mut stored, &DEFAULT_RULES, diagnostics) {
        return None;
    }
    Some(Box::new(stored))
}

/// Recursive check over the allowed value shapes. Each arm that
/// accepts a node marks its span through `rules`, so accepting and
/// marking cannot drift apart. Every shape is side-effect-free, so
/// the value is the same wherever it is resolved.
///
/// Diagnostics use the span captured before marking, so they point
/// at the declaration like any other error.
fn check_shape(expr: &mut Expr, rules: &ShapeRules, diagnostics: &mut Vec<Diagnostic>) -> bool {
    let span = expr.span;
    expr.span = rules.mark(span);
    match &mut expr.kind {
        ExprKind::BinaryLiteral { segments } => {
            let mut ok = true;
            for segment in segments {
                segment.span = rules.mark(segment.span);
                if matches!(
                    &segment.value.kind,
                    ExprKind::Literal { .. } | ExprKind::String { .. }
                ) {
                    ok &= check_shape(&mut segment.value, rules, diagnostics);
                } else {
                    diagnostics.push(Diagnostic::error(
                        format!(
                            "binary segment values in {} must be literals",
                            rules.subject
                        ),
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
            mark_names(type_path, rules);
            variant.span = rules.mark(variant.span);
            match data {
                EnumConstructionData::Struct(fields) => {
                    check_field_inits(fields, rules, diagnostics)
                }
                EnumConstructionData::Tuple(elements) => check_all(elements, rules, diagnostics),
                EnumConstructionData::Unit => true,
            }
        }
        // A constant read in its bare (`MAX`) or lowercase-leaf
        // (`Pkg.limit`) spelling. Resolve rejects a name that is not
        // a constant, and resolves in the declaring package with no
        // locals, so the value is the same at every site.
        ExprKind::Ident { .. } => true,
        kind @ ExprKind::FieldAccess { .. } if static_dotted_path(kind).is_some() => {
            mark_path(kind, rules);
            true
        }
        ExprKind::Group { expr: inner } => check_shape(inner, rules, diagnostics),
        ExprKind::List { elements } => check_all(elements, rules, diagnostics),
        ExprKind::Literal { .. } => true,
        ExprKind::Map { entries } => entries.iter_mut().fold(true, |ok, (key, value)| {
            let key_ok = check_shape(key, rules, diagnostics);
            let value_ok = check_shape(value, rules, diagnostics);
            key_ok && value_ok && ok
        }),
        ExprKind::String { parts, .. } => {
            let interpolated = parts
                .iter()
                .any(|part| matches!(part, StringPart::Interpolation { .. }));
            if interpolated {
                diagnostics.push(Diagnostic::error(
                    format!("interpolated strings are not allowed in {}", rules.subject),
                    span,
                ));
            }
            !interpolated
        }
        ExprKind::StructConstruction { type_path, fields } => {
            mark_names(type_path, rules);
            check_field_inits(fields, rules, diagnostics)
        }
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => check_shape(operand, rules, diagnostics),
        _ => {
            diagnostics.push(Diagnostic::error(
                format!(
                    "{} are limited to literals, negated numerics, enum variants, constants, \
                     binary literals, and struct, list, map, or set literals of those",
                    rules.subject
                ),
                span,
            ));
            false
        }
    }
}

/// Check every element, reporting each failure rather than stopping
/// at the first.
fn check_all(elements: &mut [Expr], rules: &ShapeRules, diagnostics: &mut Vec<Diagnostic>) -> bool {
    elements.iter_mut().fold(true, |ok, element| {
        check_shape(element, rules, diagnostics) && ok
    })
}

/// Check every field value and mark each init's own span and name.
fn check_field_inits(
    fields: &mut [FieldInit],
    rules: &ShapeRules,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    fields.iter_mut().fold(true, |ok, field| {
        field.span = rules.mark(field.span);
        field.name.span = rules.mark(field.name.span);
        check_shape(&mut field.value, rules, diagnostics) && ok
    })
}

fn mark_names(names: &mut [Name], rules: &ShapeRules) {
    for name in names {
        name.span = rules.mark(name.span);
    }
}

/// Mark every field name and receiver below the root of a static
/// dotted path (`Pkg.limit`). The chain is `Ident` and `FieldAccess`
/// nodes only, which [`static_dotted_path`] has already confirmed.
fn mark_path(kind: &mut ExprKind, rules: &ShapeRules) {
    if let ExprKind::FieldAccess { receiver, field } = kind {
        field.span = rules.mark(field.span);
        receiver.span = rules.mark(receiver.span);
        mark_path(&mut receiver.kind, rules);
    }
}
