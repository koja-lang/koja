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

use expo_ast::ast::{Diagnostic, Item, ProtocolMethod};
use expo_ast::identifier::{GlobalRegistryId, Identifier};

use crate::program::CheckedPackage;
use crate::registry::GlobalRegistry;

mod functions;
mod impls;
mod protocols;
mod structs;
mod types;

pub(crate) use types::resolve_type_expr;

/// `protocol_id -> method_name -> protocol method with default body`.
/// Local to one `lift_signatures` call.
pub(super) type ProtocolBodies = HashMap<GlobalRegistryId, HashMap<String, ProtocolMethod>>;

/// Whether a function being lifted may declare a `self` receiver. When
/// `Struct(_)`, [`functions::lift_param`] lifts `Param::Self_` to a
/// real [`crate::registry::ResolvedParam`] typed by the enclosing
/// struct and marks the signature as
/// [`crate::registry::Dispatch::Instance`].
#[derive(Clone, Copy)]
pub(super) enum SelfContext<'a> {
    None,
    Struct(&'a Identifier),
}

pub(crate) fn lift_signatures(
    packages: &mut [CheckedPackage],
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let bodies = collect_protocol_bodies(packages, registry);
    // Pass 1: protocols, structs, top-level functions. Impls run in
    // pass 2 so trait-impl conformance can rely on every protocol /
    // struct in the program being fully lifted, even across files.
    for pkg in packages.iter() {
        for file in &pkg.files {
            for item in &file.items {
                match item {
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
                    Item::Protocol(decl) => {
                        protocols::lift_protocol(decl, &pkg.package, registry, diagnostics);
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
