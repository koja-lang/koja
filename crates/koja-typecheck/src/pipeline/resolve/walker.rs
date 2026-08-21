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
//! Statement-level dispatch lives in [`super::statements`], expression
//! dispatch in [`super::expr`]. Both take a [`Resolver`] context that
//! bundles the in-scope package, the global registry, and the
//! per-function [`LocalScope`] so identifier resolution can stamp
//! [`Resolution::Local`] without re-walking.
//!
//! [`FunctionSignature`]: crate::registry::FunctionSignature
//! [`LocalId`]: koja_ast::identifier::LocalId
//! [`Param.local_id`]: koja_ast::ast::Param
//! [`Resolution::Local`]: koja_ast::identifier::Resolution::Local

use koja_ast::ast::{Diagnostic, File, Function, ImplBlock, ImplMember, Item, Param, Statement};
use koja_ast::identifier::{GlobalRegistryId, Identifier, ResolvedType};

use crate::pipeline::aliases::collect_file_aliases;
use crate::pipeline::collect::nominal_target_path;
use crate::pipeline::lift_signatures::{ResolutionScope, resolve_target_bounds};
use crate::pipeline::local_scope::LocalScope;
use crate::registry::{BoundOverlay, FunctionSignature, GlobalKind, GlobalRegistry};

use super::ctx::{Resolver, ResolverEnv};
use super::error_channel::{
    channel_for_signature, hand_wrapped_result, is_fail_statement, ok_wrap_return,
    resolve_fail_statement, return_site_expected,
};
use super::expr::resolve_expr_with_expected;
use super::field_defaults::{resolve_enum_defaults, resolve_struct_defaults};
use super::return_type::{check_explicit_return, check_return_type};
use super::statements::{resolve_assignment, resolve_compound_assignment, resolve_destructure};

pub(crate) fn resolve_file(
    file: &mut File,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let aliases = collect_file_aliases(file);
    let mut env = ResolverEnv {
        bound_overlay: None,
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
                resolve_struct_defaults(decl, &env, diagnostics);
                let enclosing_type_id = enclosing_type_id(env.package, &decl.path, env.registry);
                for function in &mut decl.functions {
                    let identifier = Identifier::member(env.package, &decl.path, &function.name);
                    resolve_function(
                        function,
                        &identifier,
                        Some(&decl.path),
                        enclosing_type_id,
                        &mut env,
                        diagnostics,
                    );
                }
            }
            Item::Builtin(decl) => {
                let enclosing_type_id = enclosing_type_id(env.package, &decl.path, env.registry);
                for function in &mut decl.functions {
                    let identifier = Identifier::member(env.package, &decl.path, &function.name);
                    resolve_function(
                        function,
                        &identifier,
                        Some(&decl.path),
                        enclosing_type_id,
                        &mut env,
                        diagnostics,
                    );
                }
            }
            Item::Enum(decl) => {
                resolve_enum_defaults(decl, &env, diagnostics);
                let enclosing_type_id = enclosing_type_id(env.package, &decl.path, env.registry);
                for function in &mut decl.functions {
                    let identifier = Identifier::member(env.package, &decl.path, &function.name);
                    resolve_function(
                        function,
                        &identifier,
                        Some(&decl.path),
                        enclosing_type_id,
                        &mut env,
                        diagnostics,
                    );
                }
            }
            // Lift's constants pass already resolved each `Constant.value`
            // (literals + struct/enum-of-literals only, no idents in
            // scope inside a constant). Walker skips them so seal's
            // assertions are the next thing they hit.
            Item::Constant(_) => {}
            Item::Impl(impl_block) => {
                // Resolve walks the methods on every shape `lift_signatures`
                // accepts (`impl X` and `impl X<...>`) so every param gets
                // a `LocalId` stamped. IR lower panics on a missing one
                // when mono later re-lowers a substituted copy of the body.
                // Identifiers anchor at the target type's package so a
                // cross-package `impl P for String` resolves its methods
                // under `Global.String.*` where collect registered them.
                let Some(path) = nominal_target_path(&impl_block.target) else {
                    continue;
                };
                let Some((_, target_package, target_path)) =
                    env.registry.lookup_owner_path(path, env.package)
                else {
                    continue;
                };
                let enclosing_type_id =
                    enclosing_type_id(&target_package, &target_path, env.registry);
                env.bound_overlay = impl_bound_overlay(impl_block, enclosing_type_id, &env);
                for member in &mut impl_block.members {
                    if let ImplMember::Function(function) = member {
                        let identifier = Identifier::member(
                            target_package.as_str(),
                            &target_path,
                            &function.name,
                        );
                        resolve_function(
                            function,
                            &identifier,
                            Some(&target_path),
                            enclosing_type_id,
                            &mut env,
                            diagnostics,
                        );
                    }
                }
                env.bound_overlay = None;
            }
            Item::Extend(extend_block) => {
                // Same as the Impl arm above, but routed to the target
                // type's package: an `extend Net.TCPSocket` block in
                // package `User` registers its methods under
                // `Net.TCPSocket.*`, so the resolver has to anchor
                // identifiers there too.
                let Some(path) = nominal_target_path(&extend_block.target) else {
                    continue;
                };
                let Some((_, target_package, target_path)) =
                    env.registry.lookup_owner_path(path, env.package)
                else {
                    continue;
                };
                let enclosing_type_id =
                    enclosing_type_id(&target_package, &target_path, env.registry);
                for member in &mut extend_block.members {
                    if let ImplMember::Function(function) = member {
                        let identifier = Identifier::member(
                            target_package.as_str(),
                            &target_path,
                            &function.name,
                        );
                        resolve_function(
                            function,
                            &identifier,
                            Some(&target_path),
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
        let mut resolver = env.make_resolver(None, None, &[], &mut scope);
        // Scripts have no return channel: a bare `return` is a normal
        // early exit and a valued `return` is rejected (exit codes go
        // through `Kernel.exit`). Treating the body as Unit-returning
        // lets `check_explicit_return` enforce that.
        resolver.current_return_type = Some(resolver.registry.primitive("Unit"));
        resolver.in_script_body = true;
        for stmt in body.iter_mut() {
            resolve_statement(stmt, &mut resolver, diagnostics);
        }
    }
}

/// Rebuild a conditional impl's [`BoundOverlay`] for its members'
/// body resolution. Bound names re-resolve into a throwaway sink
/// because lift already diagnosed any unresolvable ones.
fn impl_bound_overlay(
    impl_block: &ImplBlock,
    enclosing_type_id: Option<GlobalRegistryId>,
    env: &ResolverEnv<'_>,
) -> Option<BoundOverlay> {
    if impl_block.target_bounds.is_empty() {
        return None;
    }
    let owner = enclosing_type_id?;
    let scope = ResolutionScope {
        aliases: env.file_aliases,
        package: env.package,
        registry: env.registry,
    };
    let mut sink = Vec::new();
    let bounds = resolve_target_bounds(
        &impl_block.target,
        &impl_block.target_bounds,
        scope,
        &mut sink,
    );
    Some(BoundOverlay { bounds, owner })
}

/// Look up the registry id for a type declared in `package` with
/// the bare name `name`. Used by [`resolve_file`] to capture the
/// enclosing type's id once per decl / impl block (rather than
/// once per method) so the resolver can anchor `priv fn`
/// type-private checks. `None` when collect dropped the type.
/// Body resolution proceeds best-effort regardless.
fn enclosing_type_id(
    package: &str,
    path: &[String],
    registry: &GlobalRegistry,
) -> Option<GlobalRegistryId> {
    let identifier = Identifier::new(package, path.to_vec());
    registry.lookup(&identifier).map(|(id, _)| id)
}

fn resolve_function(
    function: &mut Function,
    identifier: &Identifier,
    enclosing_type: Option<&[String]>,
    enclosing_type_id: Option<GlobalRegistryId>,
    env: &mut ResolverEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let signature = lifted_signature(identifier, env.registry).cloned();
    let mut scope = LocalScope::new();
    if let Some(signature) = &signature {
        seed_scope_with_params(function, signature, &mut scope);
    }
    let type_param_owners = type_param_owners(identifier, function, enclosing_type, env.registry);

    let Some(body) = function.body.as_mut() else {
        return;
    };
    {
        let mut resolver = env.make_resolver(
            enclosing_type,
            enclosing_type_id,
            &type_param_owners,
            &mut scope,
        );
        resolver.error_channel = signature
            .as_ref()
            .and_then(|sig| channel_for_signature(sig, resolver.registry));
        // A `! E` function's body checks against the unwrapped
        // success type `T`. The `Result.Ok` wrapping happens after
        // each site passes (`ok_wrap_return` below,
        // `check_return_type` for the trailing expression).
        let expected = match &resolver.error_channel {
            Some(channel) if channel.ok_wraps => Some(channel.ok.clone()),
            _ => signature
                .as_ref()
                .filter(|sig| sig.return_type.is_resolved())
                .map(|sig| sig.return_type.clone()),
        };
        resolver.current_return_type = expected.clone();
        // A hand-written `Result.Ok(...)` trailer retargets at the
        // full `Result` type so `check_return_type` can teach the
        // auto-wrap rule instead of failing `E` inference.
        let trailing_expected = match (body.last(), &resolver.error_channel) {
            (Some(Statement::Expr(trailing)), Some(channel))
                if hand_wrapped_result(trailing, channel, resolver.registry) =>
            {
                Some(channel.result.clone())
            }
            _ => expected,
        };
        resolve_body_with_expected(body, trailing_expected.as_ref(), &mut resolver, diagnostics);
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
    enclosing_type: Option<&[String]>,
    registry: &GlobalRegistry,
) -> Vec<GlobalRegistryId> {
    let mut owners = Vec::new();
    if !function.type_params.is_empty()
        && let Some((fn_id, _)) = registry.lookup(identifier)
    {
        owners.push(fn_id);
    }
    if let Some(path) = enclosing_type {
        let receiver = Identifier::new(identifier.package(), path.to_vec());
        if let Some((receiver_id, _)) = registry.lookup(&receiver) {
            owners.push(receiver_id);
        }
    }
    owners
}

/// Pull the lifted signature for `identifier` out of the registry, or
/// return `None` if `collect` rejected the function or `lift_signatures`
/// hasn't stamped one (both are diagnosed upstream, body resolution
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
/// order in `function.params` matches `signature.params`. Lift
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
    // Statement-position `fail` (including match arm tails, whose
    // bodies route through here) rewrites the whole statement to
    // `return Result.Err(...)`, so it dispatches before the
    // per-shape arms below.
    if is_fail_statement(stmt) {
        resolve_fail_statement(stmt, resolver, diagnostics);
        return;
    }
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
                    "`break` is only valid inside `loop` or `while`",
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
        Statement::Destructure { pattern, value, .. } => {
            resolve_destructure(pattern, value, resolver, diagnostics);
        }
        Statement::Expr(expr) => {
            resolve_expr_with_expected(expr, expected, resolver, diagnostics);
        }
        Statement::Return { value, span } => {
            if let Some(value) = value {
                let expected =
                    return_site_expected(value, resolver.current_return_type.as_ref(), resolver);
                resolve_expr_with_expected(value, expected.as_ref(), resolver, diagnostics);
            }
            check_explicit_return(value.as_mut(), *span, resolver, diagnostics);
            ok_wrap_return(value, *span, resolver);
        }
    }
}

/// Walk every statement in `body`, resolving the trailing
/// `Statement::Expr` (if any) with `expected` as a downward type
/// hint. Non-trailing statements always resolve without an
/// expected-type hint. Only the value-producing tail matters for
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
