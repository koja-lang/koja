//! Top-down traversal: walk every package, file, function body, and
//! script-mode top-level statement, dispatching to the expression
//! resolver as it goes. Statement-level forms that don't yet carry a
//! resolved value (assignments, breaks, returns) just thread through
//! to [`super::expr::resolve_expr`] for any expressions they wrap.

use expo_ast::ast::{Diagnostic, File, Function, ImplMember, Item, Statement};

use crate::registry::GlobalRegistry;

use super::expr::resolve_expr;

pub(crate) fn resolve_file(
    file: &mut File,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for item in &mut file.items {
        match item {
            Item::Function(function) => {
                resolve_function(function, package, registry, diagnostics);
            }
            Item::Struct(decl) => {
                for function in &mut decl.functions {
                    resolve_function(function, package, registry, diagnostics);
                }
            }
            Item::Impl(impl_block) => {
                for member in &mut impl_block.members {
                    if let ImplMember::Function(function) = member {
                        resolve_function(function, package, registry, diagnostics);
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(body) = file.body.as_mut() {
        for stmt in body.iter_mut() {
            resolve_statement(stmt, package, registry, diagnostics);
        }
    }
}

fn resolve_function(
    function: &mut Function,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(body) = function.body.as_mut() else {
        return;
    };
    for stmt in body.iter_mut() {
        resolve_statement(stmt, package, registry, diagnostics);
    }
}

/// Resolve a single statement. `pub(super)` so [`super::control_flow`]
/// can recurse into `if` / `unless` body statements without re-entering
/// the file-level walker.
pub(super) fn resolve_statement(
    stmt: &mut Statement,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match stmt {
        Statement::Assignment { value, .. } | Statement::CompoundAssign { value, .. } => {
            resolve_expr(value, package, registry, diagnostics);
        }
        Statement::Break { .. } => {}
        Statement::Expr(expr) => {
            resolve_expr(expr, package, registry, diagnostics);
        }
        Statement::Return { value, .. } => {
            if let Some(value) = value {
                resolve_expr(value, package, registry, diagnostics);
            }
        }
    }
}
