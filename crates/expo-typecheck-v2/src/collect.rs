//! Collect sub-pass: walks each `File`'s top-level items and registers
//! a canonical [`Identifier`] for every globally-named decl into the
//! [`GlobalRegistry`]. Pure registration — no signature resolution
//! happens here. Resolved param/return types are annotated on the AST
//! by [`crate::lift_signatures`] once the registry is fully populated.
//!
//! The path encoding follows the [`Identifier`] convention: top-level
//! functions register at `path = ["name"]`; methods on `User` register
//! at `path = ["User", "name"]` (when impls land).
//!
//! For the POC the only registered shape is `fn` items.

use expo_ast::ast::{Diagnostic, File, Function, Item};
use expo_ast::identifier::Identifier;

use crate::labels::{item_label, item_span};
use crate::registry::GlobalRegistry;

pub(crate) fn collect_file(
    file: &File,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for item in &file.items {
        match item {
            Item::Function(function) => {
                register_function(function, package, registry, diagnostics);
            }
            // The other Item variants land as v2 grows. Reject them
            // explicitly so the POC fails loudly on shapes it cannot
            // round-trip yet.
            Item::Alias(_)
            | Item::Constant(_)
            | Item::Enum(_)
            | Item::Impl(_)
            | Item::Protocol(_)
            | Item::Shared(_)
            | Item::Struct(_)
            | Item::TypeAlias(_) => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "v2 typecheck POC does not yet support `{}` items",
                        item_label(item)
                    ),
                    item_span(item),
                ));
            }
        }
    }
}

fn register_function(
    function: &Function,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(package, vec![function.name.clone()]);
    if let Some(existing) = registry.insert_function(identifier.clone(), function.span) {
        diagnostics.push(Diagnostic::error_with_hint(
            format!("`{identifier}` is already defined"),
            format!(
                "previous {} definition is at line {}",
                existing.kind_label(),
                existing.span().start.line
            ),
            function.span,
        ));
    }
}
