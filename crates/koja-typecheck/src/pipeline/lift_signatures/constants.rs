//! Constant lifting resolves the optional `: Type` annotation on
//! `const NAME[: Type] = expr`, checks the value's shape, resolves
//! the value, and registers the
//! [`crate::registry::ConstantDefinition`] on the constant entry.
//!
//! A constant value has the field-default grammar, checked by
//! [`check_constant_shape`], and resolves through the body resolver
//! in the declaring file's scope with no locals, the same way a
//! field default does. Resolve never visits these expressions again
//! (the walker skips `Item::Constant`). Lift owns the resolution, so
//! seal can verify `Constant(Some(_))` without re-walking the AST.
//! [`super::constant_order`] decides the order constants lift in.

use koja_ast::ast::{Constant, Diagnostic, name_texts};
use koja_ast::identifier::Identifier;

use crate::pipeline::resolve::coercion::{Mismatch, check_compatible_stamping};
use crate::pipeline::resolve::resolve_in_declaring_scope;
use crate::registry::{ConstantDefinition, GlobalKind};

use super::LiftScope;
use super::field_defaults::check_constant_shape;
use super::types::{TypeParamScope, render_resolved, resolve_type_expr};

pub(super) fn lift_constant(
    constant: &mut Constant,
    scope: &mut LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(scope.package, name_texts(&constant.path));
    let Some((id, entry)) = scope.registry.lookup(&identifier) else {
        panic!(
            "lift_signatures found constant `{identifier}` missing from registry. This is a \
             collect invariant violation",
        );
    };
    // Already lifted, or the name belongs to another declaration
    // that registered first (a method on the owner, for a nested
    // constant). Collect diagnosed the collision.
    if !matches!(entry.kind, GlobalKind::Constant(None)) {
        return;
    }

    let type_params = TypeParamScope::new(&[]);
    let annotated = constant.type_annotation.as_mut().map(|type_expr| {
        resolve_type_expr(
            type_expr,
            type_params,
            scope.resolution_scope(),
            diagnostics,
        )
    });

    let inferred = if check_constant_shape(&mut constant.value, diagnostics) {
        resolve_in_declaring_scope(
            &mut constant.value,
            annotated.as_ref(),
            scope.package,
            scope.aliases,
            scope.registry,
            diagnostics,
        )
    } else {
        constant.value.resolution.clone()
    };

    if let Some(expected) = annotated.as_ref()
        && inferred.is_resolved()
        && expected.is_resolved()
    {
        match check_compatible_stamping(&mut constant.value, &inferred, expected, scope.registry) {
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
                    constant.value.span,
                ));
            }
            Some(Mismatch::Incompatible) => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "constant value type `{}` does not match annotation `{}`",
                        render_resolved(&inferred, scope.registry),
                        render_resolved(expected, scope.registry),
                    ),
                    constant.value.span,
                ));
            }
        }
    }

    // Pin the constant's stamped type at the annotation when the
    // value is a coerced literal. `inferred` is still the literal's
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
