//! Top-down traversal: walk every package, file, function body, and
//! script-mode top-level statement, dispatching to the expression
//! resolver as it goes.
//!
//! Each function body resolves against a fresh [`LocalScope`]
//! pre-populated from the function's lifted [`FunctionSignature`]:
//! every parameter becomes a [`LocalId`] entry whose name and type
//! match the lifted [`ResolvedParam`], and the AST [`Param.local_id`]
//! slot is stamped so IR lower can reach the same id without
//! re-running resolution. Script-mode `file.body` runs against its
//! own top-level scope (no params).
//!
//! Statement-level dispatch lives in [`super::statements`]; expression
//! dispatch in [`super::expr`]. Both take a [`Resolver`] context that
//! bundles the in-scope package, the global registry, and the
//! per-function [`LocalScope`] so identifier resolution can stamp
//! [`Resolution::Local`] without re-walking.
//!
//! [`FunctionSignature`]: crate::registry::FunctionSignature
//! [`LocalId`]: expo_ast::identifier::LocalId
//! [`Param.local_id`]: expo_ast::ast::Param
//! [`Resolution::Local`]: expo_ast::identifier::Resolution::Local

use expo_ast::ast::{Diagnostic, File, Function, ImplMember, Item, Param, Statement, TypeExpr};
use expo_ast::identifier::Identifier;

use crate::pipeline::local_scope::LocalScope;
use crate::registry::{FunctionSignature, GlobalKind, GlobalRegistry};

use super::ctx::Resolver;
use super::expr::resolve_expr;
use super::return_type::check_return_type;
use super::statements::resolve_assignment;

pub(crate) fn resolve_file(
    file: &mut File,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for item in &mut file.items {
        match item {
            Item::Function(function) => {
                let identifier = Identifier::new(package, vec![function.name.clone()]);
                resolve_function(function, &identifier, package, registry, diagnostics);
            }
            Item::Struct(decl) => {
                for function in &mut decl.functions {
                    let identifier =
                        Identifier::new(package, vec![decl.name.clone(), function.name.clone()]);
                    resolve_function(function, &identifier, package, registry, diagnostics);
                }
            }
            Item::Impl(impl_block) => {
                let Some(target_name) = impl_target_name(&impl_block.target) else {
                    continue;
                };
                for member in &mut impl_block.members {
                    if let ImplMember::Function(function) = member {
                        let identifier = Identifier::new(
                            package,
                            vec![target_name.to_string(), function.name.clone()],
                        );
                        resolve_function(function, &identifier, package, registry, diagnostics);
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(body) = file.body.as_mut() {
        let mut scope = LocalScope::new();
        let mut resolver = Resolver {
            package,
            registry,
            scope: &mut scope,
        };
        for stmt in body.iter_mut() {
            resolve_statement(stmt, &mut resolver, diagnostics);
        }
    }
}

fn impl_target_name(target: &TypeExpr) -> Option<&str> {
    match target {
        TypeExpr::Named { path, .. } if path.len() == 1 => Some(path[0].as_str()),
        _ => None,
    }
}

fn resolve_function(
    function: &mut Function,
    identifier: &Identifier,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let signature = lifted_signature(identifier, registry).cloned();
    let mut scope = LocalScope::new();
    if let Some(signature) = &signature {
        seed_scope_with_params(function, signature, &mut scope);
    }

    let Some(body) = function.body.as_mut() else {
        return;
    };
    let mut resolver = Resolver {
        package,
        registry,
        scope: &mut scope,
    };
    for stmt in body.iter_mut() {
        resolve_statement(stmt, &mut resolver, diagnostics);
    }

    if let Some(signature) = signature {
        check_return_type(function, &signature, registry, diagnostics);
    }
}

/// Pull the lifted signature for `identifier` out of the registry, or
/// return `None` if `collect` rejected the function or `lift_signatures`
/// hasn't stamped one (both are diagnosed upstream — body resolution
/// is best-effort but quiet here).
fn lifted_signature<'a>(
    identifier: &Identifier,
    registry: &'a GlobalRegistry,
) -> Option<&'a FunctionSignature> {
    let (_, entry) = registry.lookup(identifier)?;
    match &entry.kind {
        GlobalKind::Function(Some(signature)) => Some(signature),
        _ => None,
    }
}

/// Pre-populate `scope` with the function's params (each a fresh
/// [`LocalId`]) and stamp the freshly-minted ids back onto the AST
/// `Param.local_id` slots so IR lower can read them later. Param
/// order in `function.params` matches `signature.params`; lift
/// guarantees this even on rejected `Param::Self_` outside an `impl`
/// (an `Unresolved`-typed `ResolvedParam` is still emitted).
///
/// [`LocalId`]: expo_ast::identifier::LocalId
fn seed_scope_with_params(
    function: &mut Function,
    signature: &FunctionSignature,
    scope: &mut LocalScope,
) {
    for (param, resolved) in function.params.iter_mut().zip(signature.params.iter()) {
        let local_id = scope.declare(&resolved.name, resolved.ty.clone());
        match param {
            Param::Regular { local_id: slot, .. } | Param::Self_ { local_id: slot, .. } => {
                *slot = Some(local_id)
            }
        }
    }
}

/// Resolve a single statement. `pub(super)` so [`super::control_flow`]
/// and [`super::statements`] can recurse into nested bodies without
/// re-entering the file-level walker.
pub(super) fn resolve_statement(
    stmt: &mut Statement,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match stmt {
        Statement::Assignment {
            target,
            type_annotation,
            value,
            span,
        } => {
            resolve_assignment(
                target,
                type_annotation.as_ref(),
                value,
                *span,
                resolver,
                diagnostics,
            );
        }
        Statement::Break { .. } => {}
        Statement::CompoundAssign { span, .. } => {
            diagnostics.push(Diagnostic::error(
                "alpha typecheck does not yet support compound assignment (`+=`, `-=`, `*=`, `/=`)",
                *span,
            ));
        }
        Statement::Expr(expr) => {
            resolve_expr(expr, resolver, diagnostics);
        }
        Statement::Return { value, .. } => {
            if let Some(value) = value {
                resolve_expr(value, resolver, diagnostics);
            }
        }
    }
}
