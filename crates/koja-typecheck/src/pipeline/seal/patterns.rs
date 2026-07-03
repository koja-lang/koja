//! Pattern-shape seal checks. Supported patterns are leaves
//! (wildcards, literals, bindings, which must carry a stamped
//! `LocalId`) plus the structural shapes resolve admits
//! (`EnumUnit` / `EnumTuple` / `EnumStruct` / `Or` / `Struct`).
//! Every other shape is a feature-gap diagnostic in resolve and
//! never reaches seal on the success path.

use koja_ast::ast::{BinarySegment, ExprKind, Pattern};
use koja_ast::identifier::Resolution;
use koja_ast::labels::{pattern_kind_label, pattern_span};
use koja_ast::span::Span;

use super::seal_panic;

pub(super) fn seal_pattern(pattern: &Pattern) {
    match pattern {
        Pattern::Binding {
            local_id,
            name,
            span,
        } => {
            if local_id.is_none() {
                seal_panic(
                    &format!(
                        "match binding `{name}` carries no LocalId; resolver should have \
                         stamped it on the success path",
                    ),
                    *span,
                );
            }
        }
        Pattern::EnumStruct {
            fields,
            type_path,
            variant,
            span,
            ..
        } => {
            seal_enum_path(type_path, variant, *span);
            for field in fields {
                seal_pattern(&field.pattern);
            }
        }
        Pattern::EnumTuple {
            elements,
            type_path,
            variant,
            span,
            ..
        } => {
            seal_enum_path(type_path, variant, *span);
            for element in elements {
                seal_pattern(element);
            }
        }
        Pattern::EnumUnit {
            type_path,
            variant,
            span,
            ..
        } => seal_enum_path(type_path, variant, *span),
        Pattern::Literal { .. } | Pattern::Wildcard { .. } => {}
        Pattern::Or { patterns, span } => {
            if patterns.is_empty() {
                seal_panic("or-pattern carries no alternatives", *span);
            }
            for alternative in patterns {
                seal_pattern(alternative);
            }
        }
        Pattern::Struct {
            fields,
            type_path,
            span,
            ..
        } => {
            if type_path.is_empty() {
                seal_panic("struct pattern carries an empty type path", *span);
            }
            for field in fields {
                seal_pattern(&field.pattern);
            }
        }
        Pattern::Binary { segments, .. } => {
            for segment in segments {
                seal_binary_segment(segment);
            }
        }
        Pattern::TypedBinding {
            local_id,
            name,
            resolved_type,
            span,
            ..
        } => {
            if local_id.is_none() {
                seal_panic(
                    &format!(
                        "typed-binding pattern `{name}` carries no LocalId; resolver should \
                         have stamped it on the success path",
                    ),
                    *span,
                );
            }
            if resolved_type.is_none() {
                seal_panic(
                    &format!(
                        "typed-binding pattern `{name}` carries no resolved_type; resolver \
                         should have stamped it on the success path",
                    ),
                    *span,
                );
            }
        }
        other => seal_panic(
            &format!(
                "typecheck seal does not yet recognize pattern kind `{}`",
                pattern_kind_label(other),
            ),
            pattern_span(other),
        ),
    }
}

/// Binary-pattern segments come in three shapes after resolve:
/// literals (int / negated int / string), the discard wildcard
/// `_`, and identifier bindings, the last of which must carry a
/// `Resolution::Local` on the value expression (the resolver
/// stamps the local id in place of the parser's default
/// `Resolution::Unresolved`). Everything else is unreachable here
/// because the resolver diagnoses unsupported shapes.
fn seal_binary_segment(segment: &BinarySegment) {
    if let ExprKind::Ident { name, resolution } = &segment.value.kind
        && name != "_"
        && !matches!(resolution, Resolution::Local(_))
    {
        seal_panic(
            &format!(
                "binary pattern binding `{name}` carries no LocalId; resolver should \
                 have stamped it on the success path",
            ),
            segment.span,
        );
    }
}

fn seal_enum_path(type_path: &[String], variant: &str, span: Span) {
    if type_path.is_empty() {
        seal_panic(
            &format!("enum pattern `{variant}` carries an empty type path"),
            span,
        );
    }
    if variant.is_empty() {
        seal_panic("enum pattern carries an empty variant name", span);
    }
}
