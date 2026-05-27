//! Enum lifting: stamp the [`crate::registry::EnumDefinition`] from
//! the AST `EnumDecl` and lift inline static / instance method
//! signatures. Mirrors [`super::structs::lift_struct`] — every payload
//! `TypeExpr` resolves through the same [`super::types::resolve_type_expr`]
//! used for struct fields and function params.
//!
//! Empty `Tuple()` and `Struct {}` payloads diagnose here so the IR
//! and LLVM layers never see an empty non-`Unit` payload. The `Unit`
//! variant shape (`Red`) is the canonical "no payload" form; an
//! empty tuple or struct is a parse-shape that doesn't carry useful
//! information beyond what `Unit` already captures.

use std::collections::BTreeMap;

use koja_ast::ast::{Diagnostic, EnumDecl, EnumVariantData};
use koja_ast::identifier::Identifier;

use crate::registry::{
    EnumDefinition, GlobalKind, ResolvedEnumVariant, ResolvedStructField, ResolvedVariantData,
};

use super::LiftScope;
use super::SelfContext;
use super::functions::lift_function_with_identifier;
use super::types::{TypeParamScope, resolve_type_expr};

pub(super) fn lift_enum(
    decl: &EnumDecl,
    scope: &mut LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    lift_enum_definition(decl, scope, diagnostics);
    let enum_identifier = Identifier::new(scope.package, vec![decl.name.clone()]);
    for function in &decl.functions {
        let method_identifier = Identifier::new(
            scope.package,
            vec![decl.name.clone(), function.name.clone()],
        );
        lift_function_with_identifier(
            function,
            method_identifier,
            SelfContext::Receiver {
                receiver: &enum_identifier,
                self_override: None,
            },
            scope,
            diagnostics,
        );
    }
}

fn lift_enum_definition(
    decl: &EnumDecl,
    scope: &mut LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(scope.package, vec![decl.name.clone()]);
    let Some((id, entry)) = scope.registry.lookup(&identifier) else {
        panic!(
            "lift_signatures: enum `{identifier}` missing from registry — \
             collect invariant violation",
        );
    };
    if matches!(entry.kind, GlobalKind::Enum(Some(_))) {
        // Duplicate decl is already diagnosed by `collect`; the
        // first one stamped its definition. Skip to avoid tripping
        // `set_enum_definition`'s panic-on-double-set invariant.
        return;
    }

    let owners = if decl.type_params.is_empty() {
        Vec::new()
    } else {
        vec![id]
    };
    let type_params = TypeParamScope::new(&owners);

    let mut variants = Vec::with_capacity(decl.variants.len());
    for variant in &decl.variants {
        let data = match &variant.data {
            EnumVariantData::Struct(fields) => {
                if fields.is_empty() {
                    diagnostics.push(Diagnostic::error(
                        format!(
                            "typecheck does not support empty struct variants — \
                             use `{}` (a unit variant) instead of `{} {{}}`",
                            variant.name, variant.name,
                        ),
                        variant.span,
                    ));
                }
                let mut resolved = Vec::with_capacity(fields.len());
                for field in fields {
                    let ty = resolve_type_expr(
                        &field.type_expr,
                        type_params,
                        scope.resolution_scope(),
                        diagnostics,
                    );
                    resolved.push(ResolvedStructField {
                        name: field.name.clone(),
                        ty,
                    });
                }
                ResolvedVariantData::Struct(resolved)
            }
            EnumVariantData::Tuple(types) => {
                if types.is_empty() {
                    diagnostics.push(Diagnostic::error(
                        format!(
                            "typecheck does not support empty tuple variants — \
                             use `{}` (a unit variant) instead of `{}()`",
                            variant.name, variant.name,
                        ),
                        variant.span,
                    ));
                }
                let resolved = types
                    .iter()
                    .map(|ty| {
                        resolve_type_expr(ty, type_params, scope.resolution_scope(), diagnostics)
                    })
                    .collect();
                ResolvedVariantData::Tuple(resolved)
            }
            EnumVariantData::Unit => ResolvedVariantData::Unit,
        };
        variants.push(ResolvedEnumVariant {
            data,
            name: variant.name.clone(),
        });
    }
    scope.registry.set_enum_definition(
        id,
        EnumDefinition {
            variants,
            conformances: BTreeMap::new(),
        },
    );
}
