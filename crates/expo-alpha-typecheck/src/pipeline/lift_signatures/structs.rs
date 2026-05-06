//! Struct lifting: stamp the [`crate::registry::StructDefinition`]
//! from the AST `StructDecl` and lift inline static / instance method
//! signatures.

use expo_ast::ast::{Diagnostic, StructDecl};
use expo_ast::identifier::Identifier;

use crate::registry::{GlobalKind, GlobalRegistry, ResolvedStructField, StructDefinition};

use super::SelfContext;
use super::functions::lift_function_with_identifier;
use super::types::resolve_type_expr;

pub(super) fn lift_struct(
    decl: &StructDecl,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    lift_struct_definition(decl, package, registry, diagnostics);
    let struct_identifier = Identifier::new(package, vec![decl.name.clone()]);
    for function in &decl.functions {
        let method_identifier =
            Identifier::new(package, vec![decl.name.clone(), function.name.clone()]);
        lift_function_with_identifier(
            function,
            method_identifier,
            SelfContext::Receiver(&struct_identifier),
            package,
            registry,
            diagnostics,
        );
    }
}

fn lift_struct_definition(
    decl: &StructDecl,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(package, vec![decl.name.clone()]);
    let Some((id, entry)) = registry.lookup(&identifier) else {
        panic!(
            "lift_signatures: struct `{identifier}` missing from registry — \
             collect invariant violation",
        );
    };
    if matches!(entry.kind, GlobalKind::Struct(Some(_))) {
        // Duplicate decl is already diagnosed by `collect`; the
        // first one stamped its definition. Skip to avoid tripping
        // `set_struct_definition`'s panic-on-double-set invariant.
        return;
    }

    let mut fields = Vec::with_capacity(decl.fields.len());
    for field in &decl.fields {
        let ty = resolve_type_expr(&field.type_expr, package, registry, diagnostics);
        fields.push(ResolvedStructField {
            name: field.name.clone(),
            ty,
        });
    }
    registry.set_struct_definition(id, StructDefinition { fields });
}
