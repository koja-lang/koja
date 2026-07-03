//! Struct lifting: stamp the [`crate::registry::StructDefinition`]
//! from the AST `StructDecl` and lift inline static / instance method
//! signatures. Generic structs (`struct Pair<T, U>`) collect their
//! `type_params` here. Field types resolve against a scope that maps
//! each name to a [`koja_ast::identifier::Resolution::TypeParam`].
//! Inline method bodies see no type-param scope yet (out of scope
//! until the generic-functions slice).

use std::collections::BTreeMap;

use koja_ast::ast::{Diagnostic, StructDecl};
use koja_ast::identifier::Identifier;

use crate::registry::{GlobalKind, ResolvedStructField, StructDefinition};

use super::LiftScope;
use super::SelfContext;
use super::functions::lift_function_with_identifier;
use super::types::{TypeParamScope, resolve_type_expr};

pub(super) fn lift_struct(
    decl: &StructDecl,
    scope: &mut LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    lift_struct_definition(decl, scope, diagnostics);
    let struct_identifier = Identifier::new(scope.package, decl.path.clone());
    for function in &decl.functions {
        let method_identifier = Identifier::member(scope.package, &decl.path, &function.name);
        lift_function_with_identifier(
            function,
            method_identifier,
            SelfContext::Receiver {
                receiver: &struct_identifier,
                self_override: None,
            },
            scope,
            diagnostics,
        );
    }
}

fn lift_struct_definition(
    decl: &StructDecl,
    scope: &mut LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(scope.package, decl.path.clone());
    let Some((id, entry)) = scope.registry.lookup(&identifier) else {
        panic!(
            "lift_signatures: struct `{identifier}` missing from registry: \
             collect invariant violation",
        );
    };
    if matches!(entry.kind, GlobalKind::Struct(Some(_))) {
        // Duplicate decl is already diagnosed by `collect`. The
        // first one stamped its definition. Skip to avoid tripping
        // `set_struct_definition`'s panic-on-double-set invariant.
        return;
    }

    // Param names live on the registry entry (stamped at collect
    // time). Resolve through the chained scope rooted at this id.
    let owners = if decl.type_params.is_empty() {
        Vec::new()
    } else {
        vec![id]
    };
    let type_params = TypeParamScope::new(&owners);

    let mut fields = Vec::with_capacity(decl.fields.len());
    for field in &decl.fields {
        let ty = resolve_type_expr(
            &field.type_expr,
            type_params,
            scope.resolution_scope(),
            diagnostics,
        );
        fields.push(ResolvedStructField {
            name: field.name.clone(),
            ty,
        });
    }
    scope.registry.set_struct_definition(
        id,
        StructDefinition {
            fields,
            conformances: BTreeMap::new(),
        },
    );
}
