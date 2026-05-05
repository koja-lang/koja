//! Collect sub-pass: register a canonical [`Identifier`] for every
//! globally-named decl. Pure registration — signature resolution lives
//! in [`super::lift_signatures`].
//!
//! Path encoding follows the [`Identifier`] convention: top-level
//! functions register at `path = ["name"]`; methods on `User` will
//! register at `path = ["User", "name"]` (when impls land).
//!
//! Today `fn` items and `struct` items register; richer shapes
//! diagnose. The struct slice supports declaration + literal
//! construction + field reads only — methods, generics, `priv`, and
//! default field values surface as feature-gap diagnostics here so
//! later passes never see those shapes.

use expo_ast::ast::{Diagnostic, File, Function, Item, StructDecl, StructField};
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
            Item::Struct(decl) => {
                register_struct(decl, package, registry, diagnostics);
            }
            // Other Item variants land as alpha grows. Reject them
            // explicitly so unsupported shapes diagnose instead of
            // round-tripping silently.
            Item::Alias(_)
            | Item::Constant(_)
            | Item::Enum(_)
            | Item::Impl(_)
            | Item::Protocol(_)
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

fn register_struct(
    decl: &StructDecl,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnose_struct_feature_gaps(decl, diagnostics);
    let identifier = Identifier::new(package, vec![decl.name.clone()]);
    match registry.insert_struct(identifier.clone(), decl.span) {
        InsertOutcome::Fresh(_) => {}
        InsertOutcome::Collision { existing } => {
            diagnostics.push(Diagnostic::error_with_hint(
                format!("`{}` is already defined", existing.identifier),
                format!(
                    "previous {} definition is at line {}",
                    existing.kind.label(),
                    existing.span.start.line
                ),
                decl.span,
            ));
        }
    }
}

/// Diagnose every feature gap on a struct decl up front so collect
/// is the single seam covering them. The struct still registers (so
/// downstream `resolve` finds it for diagnostic-friendly error
/// messages); lift_signatures stamps a permissive "best effort"
/// definition in the presence of these gaps so the surrounding
/// program shape stays accurate.
fn diagnose_struct_feature_gaps(decl: &StructDecl, diagnostics: &mut Vec<Diagnostic>) {
    if !decl.type_params.is_empty() {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support generic structs (`{}` has type parameters)",
                decl.name,
            ),
            decl.span,
        ));
    }
    if !decl.functions.is_empty() {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support functions inside `struct ... end` \
                 (on `{}`)",
                decl.name,
            ),
            decl.span,
        ));
    }
    for annotation in &decl.annotations {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support annotations on struct items \
                 (`@{}` on `{}`)",
                annotation.name, decl.name,
            ),
            annotation.span,
        ));
    }
    for field in &decl.fields {
        diagnose_struct_field_gaps(&decl.name, field, diagnostics);
    }
}

fn diagnose_struct_field_gaps(
    struct_name: &str,
    field: &StructField,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if field.default.is_some() {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha typecheck does not yet support default field values \
                 (on `{struct_name}.{}`)",
                field.name,
            ),
            field.span,
        ));
    }
}
