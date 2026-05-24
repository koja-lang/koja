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
//! [`LocalId`]: koja_ast::identifier::LocalId
//! [`Param.local_id`]: koja_ast::ast::Param
//! [`Resolution::Local`]: koja_ast::identifier::Resolution::Local

use koja_ast::ast::{Diagnostic, File, Function, ImplMember, Item, Param, PassMode, Statement};
use koja_ast::identifier::{GlobalRegistryId, Identifier, ResolvedType};

use crate::pipeline::aliases::collect_file_aliases;
use crate::pipeline::collect::extend_target_path;
use crate::pipeline::lift_signatures::impl_target_name;
use crate::pipeline::local_scope::LocalScope;
use crate::registry::{FunctionSignature, GlobalKind, GlobalRegistry};

use super::ctx::{Resolver, ResolverEnv};
use super::expr::resolve_expr_with_expected;
use super::return_type::check_return_type;
use super::statements::{resolve_assignment, resolve_compound_assignment};

pub(crate) fn resolve_file(
    file: &mut File,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let aliases = collect_file_aliases(file);
    let mut env = ResolverEnv {
        file_aliases: &aliases,
        package,
        registry,
    };
    for item in &mut file.items {
        match item {
            Item::Function(function) => {
                let identifier = Identifier::new(env.package, vec![function.name.clone()]);
                resolve_function(function, &identifier, None, None, &mut env, diagnostics);
            }
            Item::Struct(decl) => {
                let enclosing_type_id = enclosing_type_id(env.package, &decl.name, env.registry);
                for function in &mut decl.functions {
                    let identifier = Identifier::new(
                        env.package,
                        vec![decl.name.clone(), function.name.clone()],
                    );
                    resolve_function(
                        function,
                        &identifier,
                        Some(&decl.name),
                        enclosing_type_id,
                        &mut env,
                        diagnostics,
                    );
                }
            }
            Item::Enum(decl) => {
                let enclosing_type_id = enclosing_type_id(env.package, &decl.name, env.registry);
                for function in &mut decl.functions {
                    let identifier = Identifier::new(
                        env.package,
                        vec![decl.name.clone(), function.name.clone()],
                    );
                    resolve_function(
                        function,
                        &identifier,
                        Some(&decl.name),
                        enclosing_type_id,
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
                let enclosing_type_id = enclosing_type_id(env.package, &target_name, env.registry);
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
                            enclosing_type_id,
                            &mut env,
                            diagnostics,
                        );
                    }
                }
            }
            Item::Extend(extend_block) => {
                // Same as the Impl arm above, but routed to the target
                // type's package: an `extend Net.TCPSocket` block in
                // package `User` registers its methods under
                // `Net.TCPSocket.*`, so the resolver has to anchor
                // identifiers there too.
                let Some((target_package, target_name)) =
                    extend_target_path(&extend_block.target, env.package)
                else {
                    continue;
                };
                let enclosing_type_id =
                    enclosing_type_id(&target_package, target_name, env.registry);
                let target_name_owned = target_name.to_string();
                for member in &mut extend_block.members {
                    if let ImplMember::Function(function) = member {
                        let identifier = Identifier::new(
                            target_package.as_str(),
                            vec![target_name_owned.clone(), function.name.clone()],
                        );
                        resolve_function(
                            function,
                            &identifier,
                            Some(&target_name_owned),
                            enclosing_type_id,
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
        let mut resolver = env.make_resolver(None, None, None, &[], &mut scope);
        for stmt in body.iter_mut() {
            resolve_statement(stmt, &mut resolver, diagnostics);
        }
    }
}

/// Look up the registry id for a type declared in `package` with
/// the bare name `name`. Used by [`resolve_file`] to capture the
/// enclosing type's id once per decl / impl block (rather than
/// once per method) so the resolver can anchor `priv fn`
/// type-private checks. `None` when collect dropped the type
/// — body resolution proceeds best-effort regardless.
fn enclosing_type_id(
    package: &str,
    name: &str,
    registry: &GlobalRegistry,
) -> Option<GlobalRegistryId> {
    let identifier = Identifier::new(package, vec![name.to_string()]);
    registry.lookup(&identifier).map(|(id, _)| id)
}

fn resolve_function(
    function: &mut Function,
    identifier: &Identifier,
    enclosing_type: Option<&str>,
    enclosing_type_id: Option<GlobalRegistryId>,
    env: &mut ResolverEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let signature = lifted_signature(identifier, env.registry).cloned();
    let mut scope = LocalScope::new();
    if let Some(signature) = &signature {
        seed_scope_with_params(function, signature, &mut scope);
    }
    let self_pass_mode = signature.as_ref().and_then(self_param_mode);
    let type_param_owners = type_param_owners(identifier, function, enclosing_type, env.registry);

    let Some(body) = function.body.as_mut() else {
        return;
    };
    {
        let mut resolver = env.make_resolver(
            enclosing_type,
            enclosing_type_id,
            self_pass_mode,
            &type_param_owners,
            &mut scope,
        );
        let expected = signature
            .as_ref()
            .filter(|sig| sig.return_type.is_resolved())
            .map(|sig| sig.return_type.clone());
        resolver.current_return_type = expected.clone();
        resolve_body_with_expected(body, expected.as_ref(), &mut resolver, diagnostics);
    }

    if let Some(signature) = signature {
        check_return_type(function, &signature, env, diagnostics);
    }
}

/// Mirrors `lift_signatures::functions::type_param_owners` for the
/// resolve pass: chain the function's own id (when it declares
/// type-params) over the receiver type's id (when this is a method).
/// Used so in-body type annotations like `result: List<T> = ...`
/// resolve the enclosing scope's `T` / `U` correctly.
fn type_param_owners(
    identifier: &Identifier,
    function: &Function,
    enclosing_type: Option<&str>,
    registry: &GlobalRegistry,
) -> Vec<GlobalRegistryId> {
    let mut owners = Vec::new();
    if !function.type_params.is_empty()
        && let Some((fn_id, _)) = registry.lookup(identifier)
    {
        owners.push(fn_id);
    }
    if let Some(name) = enclosing_type {
        let receiver = Identifier::new(identifier.package(), vec![name.to_string()]);
        if let Some((receiver_id, _)) = registry.lookup(&receiver) {
            owners.push(receiver_id);
        }
    }
    owners
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

/// `PassMode` of the lifted signature's `self` receiver, when one
/// exists. `lift_signatures::functions` always assigns the `"self"`
/// name to a `Param::Self_`, so a name-keyed lookup avoids depending
/// on slot-0 ordering invariants and stays robust against any future
/// reshuffling of the param vec.
fn self_param_mode(signature: &FunctionSignature) -> Option<PassMode> {
    signature
        .params
        .iter()
        .find(|param| param.name == "self")
        .map(|param| param.mode)
}

/// Pre-populate `scope` with the function's params (each a fresh
/// [`LocalId`]) and stamp the freshly-minted ids back onto the AST
/// `Param.local_id` slots so IR lower can read them later. Param
/// order in `function.params` matches `signature.params`; lift
/// guarantees this even on rejected `Param::Self_` outside an `impl`
/// (an `Unresolved`-typed `ResolvedParam` is still emitted).
///
/// [`LocalId`]: koja_ast::identifier::LocalId
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
    resolve_statement_with_expected(stmt, None, resolver, diagnostics);
}

/// Like [`resolve_statement`] but threads an expected-type hint into
/// trailing-position [`Statement::Expr`]s so bidirectional shapes
/// (`Option.None` in a function returning `Option<T>`,
/// `Result.Ok(x)` whose `E` only resolves through the surrounding
/// context, …) get the surrounding type as expected. Non-`Expr`
/// statements ignore the hint.
pub(super) fn resolve_statement_with_expected(
    stmt: &mut Statement,
    expected: Option<&ResolvedType>,
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
        Statement::Break { span } => {
            if resolver.loop_depth == 0 {
                diagnostics.push(Diagnostic::error_with_hint(
                    "break outside of loop",
                    "'break' can only be used inside 'loop' or 'while'",
                    *span,
                ));
            } else if let Some(seen) = resolver.loop_break_seen.last_mut() {
                *seen = true;
            }
        }
        Statement::CompoundAssign {
            target,
            op,
            value,
            span,
        } => {
            resolve_compound_assignment(target, *op, value, *span, resolver, diagnostics);
        }
        Statement::Expr(expr) => {
            resolve_expr_with_expected(expr, expected, resolver, diagnostics);
        }
        Statement::Return { value, .. } => {
            if let Some(value) = value {
                let expected = resolver.current_return_type.clone();
                resolve_expr_with_expected(value, expected.as_ref(), resolver, diagnostics);
            }
        }
    }
}

/// Walk every statement in `body`, resolving the trailing
/// `Statement::Expr` (if any) with `expected` as a downward type
/// hint. Non-trailing statements always resolve without an
/// expected-type hint — only the value-producing tail matters for
/// bidirectional inference.
pub(super) fn resolve_body_with_expected(
    body: &mut [Statement],
    expected: Option<&ResolvedType>,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some((last, leading)) = body.split_last_mut() else {
        return;
    };
    for stmt in leading {
        resolve_statement(stmt, resolver, diagnostics);
    }
    resolve_statement_with_expected(last, expected, resolver, diagnostics);
}
