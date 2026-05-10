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

use expo_ast::ast::{Diagnostic, File, Function, ImplMember, Item, Param, Statement};
use expo_ast::identifier::Identifier;

use crate::pipeline::lift_signatures::impl_target_name;
use crate::pipeline::local_scope::LocalScope;
use crate::registry::{FunctionSignature, GlobalKind, GlobalRegistry};

use super::coercion::Coercions;
use super::ctx::{Resolver, ResolverEnv};
use super::expr::resolve_expr;
use super::return_type::check_return_type;
use super::statements::{resolve_assignment, resolve_compound_assignment};

pub(crate) fn resolve_file(
    file: &mut File,
    package: &str,
    registry: &GlobalRegistry,
    coercions: &mut Coercions,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut env = ResolverEnv {
        coercions,
        package,
        registry,
    };
    for item in &mut file.items {
        match item {
            Item::Function(function) => {
                let identifier = Identifier::new(env.package, vec![function.name.clone()]);
                resolve_function(function, &identifier, None, &mut env, diagnostics);
            }
            Item::Struct(decl) => {
                for function in &mut decl.functions {
                    let identifier = Identifier::new(
                        env.package,
                        vec![decl.name.clone(), function.name.clone()],
                    );
                    resolve_function(
                        function,
                        &identifier,
                        Some(&decl.name),
                        &mut env,
                        diagnostics,
                    );
                }
            }
            Item::Enum(decl) => {
                for function in &mut decl.functions {
                    let identifier = Identifier::new(
                        env.package,
                        vec![decl.name.clone(), function.name.clone()],
                    );
                    resolve_function(
                        function,
                        &identifier,
                        Some(&decl.name),
                        &mut env,
                        diagnostics,
                    );
                }
            }
            // Lift's constants pass already resolved each `Constant.value`
            // (literals + struct/enum-of-literals only — no idents in
            // scope inside a constant). Walker skips them so seal's
            // assertions are the next thing they hit.
            Item::Constant(_) => {}
            Item::Impl(impl_block) => {
                // Resolve walks the methods on every shape `lift_signatures`
                // accepts (`impl X` and `impl X<...>`) so every param gets
                // a `LocalId` stamped. IR lower panics on a missing one
                // when mono later re-lowers a substituted copy of the body.
                let Some(target_name) = impl_target_name(&impl_block.target) else {
                    continue;
                };
                let target_name = target_name.to_string();
                for member in &mut impl_block.members {
                    if let ImplMember::Function(function) = member {
                        let identifier = Identifier::new(
                            env.package,
                            vec![target_name.clone(), function.name.clone()],
                        );
                        resolve_function(
                            function,
                            &identifier,
                            Some(&target_name),
                            &mut env,
                            diagnostics,
                        );
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(body) = file.body.as_mut() {
        let mut scope = LocalScope::new();
        let mut resolver = env.make_resolver(None, &mut scope);
        for stmt in body.iter_mut() {
            resolve_statement(stmt, &mut resolver, diagnostics);
        }
    }
}

fn resolve_function(
    function: &mut Function,
    identifier: &Identifier,
    enclosing_type: Option<&str>,
    env: &mut ResolverEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let signature = lifted_signature(identifier, env.registry).cloned();
    let mut scope = LocalScope::new();
    if let Some(signature) = &signature {
        seed_scope_with_params(function, signature, &mut scope);
    }

    let Some(body) = function.body.as_mut() else {
        return;
    };
    {
        let mut resolver = env.make_resolver(enclosing_type, &mut scope);
        for stmt in body.iter_mut() {
            resolve_statement(stmt, &mut resolver, diagnostics);
        }
    }

    if let Some(signature) = signature {
        check_return_type(function, &signature, env, diagnostics);
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
        Statement::CompoundAssign {
            target,
            op,
            value,
            span,
        } => {
            resolve_compound_assignment(target, *op, value, *span, resolver, diagnostics);
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
