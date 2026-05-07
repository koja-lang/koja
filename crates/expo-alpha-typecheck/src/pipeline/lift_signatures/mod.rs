//! Lift-signatures sub-pass: resolve `TypeExpr`s and stamp lifted
//! payloads onto the registry — `FunctionSignature` for functions,
//! `StructDefinition` for structs, `ProtocolDefinition` for protocols.
//!
//! Runs after `collect` (each named decl has its `*(None)` slot) and
//! before `resolve` (call sites, field access, and protocol-method
//! dispatch see lifted metadata).
//!
//! Trait impls (`impl Foo for Bar`) get conformance-checked here:
//! declared method sigs must match the protocol; protocol methods
//! with default bodies that the impl omits are synthesized into the
//! impl's `members` (cloned body, `self` typed as the impl target).
//! Default bodies live in a per-invocation [`ProtocolBodies`] sidecar
//! so the registry stays a "resolved types only" surface.

use std::collections::HashMap;

use expo_ast::ast::{
    Diagnostic, EnumDecl, Function, Item, ProtocolDecl, ProtocolMethod, StructDecl,
};
use expo_ast::identifier::{GlobalRegistryId, Identifier, ResolvedType};

use crate::program::CheckedPackage;
use crate::registry::GlobalRegistry;

mod enums;
mod functions;
mod impls;
mod protocols;
mod structs;
mod types;

pub(crate) use types::{
    TypeParamScope, protocol_impl_identifier, render_type_expr, resolve_type_expr,
};

use types::resolve_bound_to_id;

/// `protocol_id -> method_name -> protocol method with default body`.
/// Local to one `lift_signatures` call.
pub(super) type ProtocolBodies = HashMap<GlobalRegistryId, HashMap<String, ProtocolMethod>>;

/// Whether a function being lifted may declare a `self` receiver
/// and how to type it. `Receiver(ident)` is the inline-method case
/// (`fn` declared inside `struct` / `enum` body or an inherent
/// `impl` block) — `self` types as `concrete_self_type(ident)`.
/// `Impl { impl_id, target }` is the trait-impl method case
/// (`impl P for T { fn ... }`) — `self` types as the resolved
/// target so generic-target impls like `impl Show for List<T>`
/// see `self: List<TypeParam(impl_id, 0)>` and the protocol-impl
/// entry's free type-params anchor at `impl_id`.
#[derive(Clone, Copy)]
pub(super) enum SelfContext<'a> {
    None,
    Receiver(&'a Identifier),
    Impl {
        impl_id: GlobalRegistryId,
        target: &'a ResolvedType,
    },
}

pub(crate) fn lift_signatures(
    packages: &mut [CheckedPackage],
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let bodies = collect_protocol_bodies(packages, registry);
    // Pass 1a: protocols. Lifted first so protocol method rosters
    // exist for the bounds-resolve sub-pass below and for trait-impl
    // conformance in pass 2.
    for pkg in packages.iter() {
        for file in &pkg.files {
            for item in &file.items {
                if let Item::Protocol(decl) = item {
                    protocols::lift_protocol(decl, &pkg.package, registry, diagnostics);
                }
            }
        }
    }
    // Pass 1b: resolve `<T: Bound>` bound names against the now-fully-
    // populated protocol set; stamp resolved ids onto every decl's
    // `RegistryEntry.type_param_bounds`. Runs after protocol lift so
    // bound names can refer to protocols declared anywhere in the
    // program; runs before struct / enum / function lift so their
    // method signatures can already enforce bounds (slice 2.3).
    resolve_all_bounds(packages, registry, diagnostics);
    // Pass 1c: structs, enums, top-level functions. Order doesn't
    // matter inside this pass — every signature resolution either
    // hits a protocol (already lifted) or another struct/enum
    // (already registered with type_params at collect).
    for pkg in packages.iter() {
        for file in &pkg.files {
            for item in &file.items {
                match item {
                    Item::Enum(decl) => {
                        enums::lift_enum(decl, &pkg.package, registry, diagnostics);
                    }
                    Item::Function(function) => {
                        let identifier = Identifier::new(&pkg.package, vec![function.name.clone()]);
                        functions::lift_function_with_identifier(
                            function,
                            identifier,
                            SelfContext::None,
                            &pkg.package,
                            registry,
                            diagnostics,
                        );
                    }
                    Item::Struct(decl) => {
                        structs::lift_struct(decl, &pkg.package, registry, diagnostics);
                    }
                    _ => {}
                }
            }
        }
    }
    // Pass 2: impls. Mutable so synthesis can push members.
    for pkg in packages.iter_mut() {
        let package = pkg.package.clone();
        for file in &mut pkg.files {
            for item in &mut file.items {
                if let Item::Impl(impl_block) = item {
                    impls::lift_impl(impl_block, &package, &bodies, registry, diagnostics);
                }
            }
        }
    }
}

/// Walk every generic-decl AST node, resolve each declared bound
/// name to a protocol id, and stamp the parallel
/// `Vec<Vec<GlobalRegistryId>>` onto its registry entry. Diagnoses
/// unresolved names and bound names that resolve to non-protocol
/// kinds via [`resolve_bound_to_id`].
fn resolve_all_bounds(
    packages: &[CheckedPackage],
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for pkg in packages {
        for file in &pkg.files {
            for item in &file.items {
                match item {
                    Item::Enum(decl) => {
                        resolve_enum_bounds(decl, &pkg.package, registry, diagnostics)
                    }
                    Item::Function(function) => resolve_function_bounds(
                        function,
                        Identifier::new(&pkg.package, vec![function.name.clone()]),
                        &pkg.package,
                        registry,
                        diagnostics,
                    ),
                    Item::Protocol(decl) => {
                        resolve_protocol_bounds(decl, &pkg.package, registry, diagnostics)
                    }
                    Item::Struct(decl) => {
                        resolve_struct_bounds(decl, &pkg.package, registry, diagnostics);
                        for function in &decl.functions {
                            resolve_function_bounds(
                                function,
                                Identifier::new(
                                    &pkg.package,
                                    vec![decl.name.clone(), function.name.clone()],
                                ),
                                &pkg.package,
                                registry,
                                diagnostics,
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn resolve_struct_bounds(
    decl: &StructDecl,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(package, vec![decl.name.clone()]);
    let Some((id, _)) = registry.lookup(&identifier) else {
        return;
    };
    let resolved = resolve_param_bounds(&decl.type_params, package, registry, diagnostics);
    registry.set_type_param_bounds(id, resolved);
}

fn resolve_enum_bounds(
    decl: &EnumDecl,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(package, vec![decl.name.clone()]);
    let Some((id, _)) = registry.lookup(&identifier) else {
        return;
    };
    let resolved = resolve_param_bounds(&decl.type_params, package, registry, diagnostics);
    registry.set_type_param_bounds(id, resolved);
    for function in &decl.functions {
        resolve_function_bounds(
            function,
            Identifier::new(package, vec![decl.name.clone(), function.name.clone()]),
            package,
            registry,
            diagnostics,
        );
    }
}

fn resolve_protocol_bounds(
    decl: &ProtocolDecl,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(package, vec![decl.name.clone()]);
    let Some((id, _)) = registry.lookup(&identifier) else {
        return;
    };
    // Protocols register with `["Self", ...declared]`. Slot 0 is
    // unbounded — the user-declared bounds line up at slots 1..N.
    let mut resolved = vec![Vec::new()];
    resolved.extend(resolve_param_bounds(
        &decl.type_params,
        package,
        registry,
        diagnostics,
    ));
    registry.set_type_param_bounds(id, resolved);
}

fn resolve_function_bounds(
    function: &Function,
    identifier: Identifier,
    package: &str,
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some((id, _)) = registry.lookup(&identifier) else {
        return;
    };
    let resolved = resolve_param_bounds(&function.type_params, package, registry, diagnostics);
    registry.set_type_param_bounds(id, resolved);
}

/// Per-decl bound resolution shared by every owner kind: each AST
/// `TypeParam`'s bound list maps to a `Vec<GlobalRegistryId>` of
/// resolved protocol ids (skipping any name that didn't resolve so
/// the diagnostic from `resolve_bound_to_id` is the user-visible
/// outcome).
fn resolve_param_bounds(
    type_params: &[expo_ast::ast::TypeParam],
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Vec<GlobalRegistryId>> {
    type_params
        .iter()
        .map(|param| {
            param
                .bounds
                .iter()
                .filter_map(|bound| {
                    resolve_bound_to_id(bound, param.span, package, registry, diagnostics)
                })
                .collect()
        })
        .collect()
}

/// Phase A: clone each protocol method's default body into the
/// sidecar so phase B can synthesize without re-walking the AST.
fn collect_protocol_bodies(
    packages: &[CheckedPackage],
    registry: &GlobalRegistry,
) -> ProtocolBodies {
    let mut bodies: ProtocolBodies = HashMap::new();
    for pkg in packages {
        for file in &pkg.files {
            for item in &file.items {
                let Item::Protocol(decl) = item else {
                    continue;
                };
                let identifier = Identifier::new(&pkg.package, vec![decl.name.clone()]);
                let Some((id, _)) = registry.lookup(&identifier) else {
                    continue;
                };
                let entry = bodies.entry(id).or_default();
                for method in &decl.methods {
                    if method.body.is_some() {
                        entry.insert(method.name.clone(), method.clone());
                    }
                }
            }
        }
    }
    bodies
}
