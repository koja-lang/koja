//! Collect sub-pass: register a canonical [`Identifier`] for every
//! globally-named decl. Pure registration — signature resolution lives
//! in [`super::lift_signatures`].
//!
//! Path encoding follows the [`Identifier`] convention: top-level
//! functions register at `path = ["name"]`; methods on `User` will
//! register at `path = ["User", "name"]` (when impls land).
//!
//! Today only `fn` items register; richer shapes diagnose.

use expo_ast::ast::{Diagnostic, File, Function, Item};
use expo_ast::identifier::Identifier;

use crate::labels::{item_label, item_span};
use crate::registry::{GlobalRegistry, InsertOutcome};

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
            // Other Item variants land as alpha grows. Reject them
            // explicitly so unsupported shapes diagnose instead of
            // round-tripping silently.
            Item::Alias(_)
            | Item::Constant(_)
            | Item::Enum(_)
            | Item::Impl(_)
            | Item::Protocol(_)
            | Item::Struct(_)
            | Item::TypeAlias(_) => {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "alpha typecheck does not yet support `{}` items",
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
    match registry.insert_function(identifier.clone(), function.span) {
        InsertOutcome::Fresh(_) => {}
        InsertOutcome::Collision { existing } => {
            diagnostics.push(Diagnostic::error_with_hint(
                format!("`{}` is already defined", existing.identifier),
                format!(
                    "previous {} definition is at line {}",
                    existing.kind.label(),
                    existing.span.start.line
                ),
                function.span,
            ));
        }
    }
}
