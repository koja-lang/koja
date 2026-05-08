//! Pattern-shape seal checks. Supported patterns are leaves
//! (wildcards, literals, bindings — which must carry a stamped
//! `LocalId`) plus the structural shapes resolve admits
//! (`EnumUnit` / `EnumTuple` / `EnumStruct` / `Or` / `Struct`).
//! Every other shape is a feature-gap diagnostic in resolve and
//! never reaches seal on the success path.

use expo_ast::ast::Pattern;
use expo_ast::labels::{pattern_kind_label, pattern_span};
use expo_ast::span::Span;

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
        other => seal_panic(
            &format!(
                "alpha typecheck seal does not yet recognize pattern kind `{}`",
                pattern_kind_label(other),
            ),
            pattern_span(other),
        ),
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
