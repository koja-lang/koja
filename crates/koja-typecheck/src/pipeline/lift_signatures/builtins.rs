//! Builtin lifting: the compiler owns a builtin's definition (the
//! [`crate::registry::BuiltinShape`] is stamped at seed time), so
//! this module only lifts inline method signatures. An unknown
//! builtin name falls back to an ordinary struct entry at collect
//! time. Its empty definition is stamped here so seal sees a
//! stamped entry.

use std::collections::BTreeMap;

use koja_ast::ast::{BuiltinDecl, Diagnostic};
use koja_ast::identifier::Identifier;

use crate::registry::{GlobalKind, StructDefinition};

use super::LiftScope;
use super::SelfContext;
use super::functions::lift_function_with_identifier;

pub(super) fn lift_builtin(
    decl: &BuiltinDecl,
    scope: &mut LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(scope.package, decl.path.clone());
    let Some((id, entry)) = scope.registry.lookup(&identifier) else {
        panic!(
            "lift_signatures: builtin `{identifier}` missing from registry: \
             collect invariant violation",
        );
    };
    if matches!(entry.kind, GlobalKind::Struct(None)) {
        scope.registry.set_struct_definition(
            id,
            StructDefinition {
                conformances: BTreeMap::new(),
                fields: Vec::new(),
            },
        );
    }
    for function in &decl.functions {
        let method_identifier = Identifier::member(scope.package, &decl.path, &function.name);
        lift_function_with_identifier(
            function,
            method_identifier,
            SelfContext::Receiver {
                receiver: &identifier,
                self_override: None,
            },
            scope,
            diagnostics,
        );
    }
}
