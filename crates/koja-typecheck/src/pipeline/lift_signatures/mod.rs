//! Lift-signatures sub-pass: resolve `TypeExpr`s and stamp lifted
//! payloads onto the registry: `FunctionSignature` for functions,
//! `StructDefinition` for structs, `ProtocolDefinition` for protocols.
//!
//! Runs after `collect` (each named decl has its `*(None)` slot) and
//! before `resolve` (call sites, field access, and protocol-method
//! dispatch see lifted metadata).
//!
//! Trait impls (`impl Foo for Bar`) get conformance-checked here:
//! declared method sigs must match the protocol. Protocol methods
//! with default bodies that the impl omits are synthesized into the
//! impl's `members` (cloned body, `self` typed as the impl target).
//! Default bodies live in a per-invocation [`ProtocolBodies`] sidecar
//! so the registry stays a "resolved types only" surface.

use std::collections::HashMap;

use koja_ast::ast::{
    AliasDecl, Diagnostic, EnumDecl, Function, Item, ProtocolDecl, ProtocolMethod, StructDecl,
};
use koja_ast::identifier::{GlobalRegistryId, Identifier, ResolvedType};

use crate::pipeline::aliases::collect_file_aliases;
use crate::program::CheckedPackage;
use crate::registry::GlobalRegistry;

mod constants;
mod enums;
mod functions;
mod impls;
mod protocols;
mod structs;
mod type_aliases;
mod types;

pub(crate) use types::{ResolutionScope, TypeParamScope, resolve_type_expr};

use types::resolve_bound_to_id;

/// Mutable counterpart to [`ResolutionScope`] used by every
/// `lift_*` function: same name-resolution inputs (alias slice,
/// current package, registry) but with a `&mut` registry so
/// signature / definition stamps land on the right entries.
///
/// **Do not grow this struct.** It exists to bundle the four
/// pieces every lifter needs to (a) look names up under the file's
/// alias rules and (b) write lifted payloads onto the registry.
/// `diagnostics` lives outside on purpose. See the doc on
/// [`ResolutionScope`] for the rationale.
///
/// Use [`Self::resolution_scope`] to drop into read-only type
/// resolution: it reborrows `registry` immutably for the duration
/// of the call, so any subsequent `&mut` write through the lift
/// scope is sequenced naturally by the borrow checker.
pub(super) struct LiftScope<'a> {
    pub aliases: &'a [AliasDecl],
    pub package: &'a str,
    pub registry: &'a mut GlobalRegistry,
}

impl<'a> LiftScope<'a> {
    /// Reborrow this lift scope as the read-only [`ResolutionScope`]
    /// expected by [`resolve_type_expr`] and friends. The aliases
    /// and package projections naturally outlive `&self` (their
    /// fields are `&'a`). The registry reborrow is `&'_`-scoped to
    /// the call so subsequent `&mut self.registry` writes typecheck.
    pub(super) fn resolution_scope(&self) -> ResolutionScope<'_> {
        ResolutionScope {
            aliases: self.aliases,
            package: self.package,
            registry: self.registry,
        }
    }
}

/// `protocol_id -> method_name -> protocol method with default body`.
/// Local to one `lift_signatures` call.
pub(super) type ProtocolBodies = HashMap<GlobalRegistryId, HashMap<String, ProtocolMethod>>;

/// Whether a function being lifted may declare a `self` receiver
/// and how to type it. `Receiver { receiver, self_override }`
/// covers every shape that has a `self`: `fn` declared inside a
/// `struct`/`enum` body, an inherent `impl` block, or a trait
/// `impl P for T` block. The type-param scope always anchors at
/// the receiver's id.
///
/// `self_override` is the trait-impl target hook. When `None`,
/// `self` types as
/// [`super::types::concrete_self_type`] of the receiver, the
/// inline / inherent path. When `Some`, `self` types as the
/// resolved target verbatim. For a generic-target impl like
/// `impl P for Bag<T>` the override is `Bag<TypeParam(Bag, 0)>`,
/// which equals `concrete_self_type(Bag)`, so the two paths
/// converge. The override only diverges when the impl pins
/// concrete args (e.g. `impl P for Bag<Int>`). Pinning `self` to
/// `Bag<Int>` is what lets call-site dispatch diagnose
/// `Bag<String>.render()` as a domain miss.
#[derive(Clone, Copy)]
pub(super) enum SelfContext<'a> {
    None,
    Receiver {
        receiver: &'a Identifier,
        self_override: Option<&'a ResolvedType>,
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
            let aliases = collect_file_aliases(file);
            let mut scope = LiftScope {
                aliases: &aliases,
                package: &pkg.package,
                registry,
            };
            for item in &file.items {
                if let Item::Protocol(decl) = item {
                    protocols::lift_protocol(decl, &mut scope, diagnostics);
                }
            }
        }
    }
    // Pass 1b: resolve `<T: Bound>` bound names against the now-fully-
    // populated protocol set. Stamp resolved ids onto every decl's
    // `RegistryEntry.type_param_bounds`. Runs after protocol lift so
    // bound names can refer to protocols declared anywhere in the
    // program. Runs before struct / enum / function lift so their
    // method signatures can already enforce bounds (slice 2.3). Each
    // file's bounds resolve against its own aliases so an aliased
    // protocol name can be used as a bound (`<T: AliasedProtocol>`).
    resolve_all_bounds(packages, registry, diagnostics);
    // Pass 1b': type aliases. Resolves each `type X = ...` RHS and
    // stamps the canonical `ResolvedType` onto the alias entry so
    // struct / enum / function signatures in pass 1c can reference
    // aliases by name. Cycle detection runs as a follow-up sweep
    // inside `lift_type_aliases`.
    type_aliases::lift_type_aliases(packages, registry, diagnostics);
    // Pass 1c: structs, enums, top-level functions. Order doesn't
    // matter inside this pass: every signature resolution either
    // hits a protocol (already lifted) or another struct/enum
    // (already registered with type_params at collect).
    for pkg in packages.iter() {
        for file in &pkg.files {
            let aliases = collect_file_aliases(file);
            let mut scope = LiftScope {
                aliases: &aliases,
                package: &pkg.package,
                registry,
            };
            for item in &file.items {
                match item {
                    Item::Enum(decl) => enums::lift_enum(decl, &mut scope, diagnostics),
                    Item::Function(function) => {
                        let identifier =
                            Identifier::new(scope.package, vec![function.name.clone()]);
                        functions::lift_function_with_identifier(
                            function,
                            identifier,
                            SelfContext::None,
                            &mut scope,
                            diagnostics,
                        );
                    }
                    Item::Struct(decl) => structs::lift_struct(decl, &mut scope, diagnostics),
                    _ => {}
                }
            }
        }
    }
    // Pass 1d: constants. Runs after structs / enums lift so the
    // constant value resolver can look up struct field layouts and
    // enum variant rosters when validating struct-of-literals and
    // unit-enum-variant RHSs. Mutable iteration mutates each
    // `Constant.value` Expr's `resolution` slots as it walks. The
    // final stamped definition clones the resolved Expr into the
    // registry so IR lower never has to re-walk file items.
    for pkg in packages.iter_mut() {
        let package = pkg.package.clone();
        for file in &mut pkg.files {
            let aliases = collect_file_aliases(file);
            let mut scope = LiftScope {
                aliases: &aliases,
                package: &package,
                registry,
            };
            for item in &mut file.items {
                if let Item::Constant(constant) = item {
                    constants::lift_constant(constant, &mut scope, diagnostics);
                }
            }
        }
    }
    // Pass 2: impl + extend blocks. Mutable so impl synthesis can
    // push members.
    for pkg in packages.iter_mut() {
        let package = pkg.package.clone();
        for file in &mut pkg.files {
            let aliases = collect_file_aliases(file);
            let mut scope = LiftScope {
                aliases: &aliases,
                package: &package,
                registry,
            };
            for item in &mut file.items {
                match item {
                    Item::Impl(impl_block) => {
                        impls::lift_impl(impl_block, &bodies, &mut scope, diagnostics);
                    }
                    Item::Extend(extend_block) => {
                        impls::lift_extend(extend_block, &mut scope, diagnostics);
                    }
                    _ => {}
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
            let aliases = collect_file_aliases(file);
            let mut scope = LiftScope {
                aliases: &aliases,
                package: &pkg.package,
                registry,
            };
            for item in &file.items {
                match item {
                    Item::Enum(decl) => resolve_enum_bounds(decl, &mut scope, diagnostics),
                    Item::Function(function) => resolve_function_bounds(
                        function,
                        Identifier::new(scope.package, vec![function.name.clone()]),
                        &mut scope,
                        diagnostics,
                    ),
                    Item::Protocol(decl) => resolve_protocol_bounds(decl, &mut scope, diagnostics),
                    Item::Struct(decl) => {
                        resolve_struct_bounds(decl, &mut scope, diagnostics);
                        for function in &decl.functions {
                            resolve_function_bounds(
                                function,
                                Identifier::member(scope.package, &decl.path, &function.name),
                                &mut scope,
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
    scope: &mut LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(scope.package, decl.path.clone());
    let Some((id, _)) = scope.registry.lookup(&identifier) else {
        return;
    };
    let resolved = resolve_param_bounds(&decl.type_params, scope.resolution_scope(), diagnostics);
    scope.registry.set_type_param_bounds(id, resolved);
}

fn resolve_enum_bounds(
    decl: &EnumDecl,
    scope: &mut LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(scope.package, decl.path.clone());
    let Some((id, _)) = scope.registry.lookup(&identifier) else {
        return;
    };
    let resolved = resolve_param_bounds(&decl.type_params, scope.resolution_scope(), diagnostics);
    scope.registry.set_type_param_bounds(id, resolved);
    for function in &decl.functions {
        resolve_function_bounds(
            function,
            Identifier::member(scope.package, &decl.path, &function.name),
            scope,
            diagnostics,
        );
    }
}

fn resolve_protocol_bounds(
    decl: &ProtocolDecl,
    scope: &mut LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let identifier = Identifier::new(scope.package, vec![decl.name.clone()]);
    let Some((id, _)) = scope.registry.lookup(&identifier) else {
        return;
    };
    // Protocols register with `["Self", ...declared]`. Slot 0 is
    // unbounded: the user-declared bounds line up at slots 1..N.
    // Skip any user-declared `Self` so the bounds vec aligns with
    // the type_params list `register_protocol` built (`Self` is
    // synthetic and a reserved name. The diagnostic for re-using it
    // already fired during collect).
    let user_params: Vec<_> = decl
        .type_params
        .iter()
        .filter(|param| param.name != "Self")
        .cloned()
        .collect();
    let mut resolved = vec![Vec::new()];
    resolved.extend(resolve_param_bounds(
        &user_params,
        scope.resolution_scope(),
        diagnostics,
    ));
    scope.registry.set_type_param_bounds(id, resolved);
}

fn resolve_function_bounds(
    function: &Function,
    identifier: Identifier,
    scope: &mut LiftScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some((id, _)) = scope.registry.lookup(&identifier) else {
        return;
    };
    let resolved =
        resolve_param_bounds(&function.type_params, scope.resolution_scope(), diagnostics);
    scope.registry.set_type_param_bounds(id, resolved);
}

/// Per-decl bound resolution shared by every owner kind: each AST
/// `TypeParam`'s bound list maps to a `Vec<GlobalRegistryId>` of
/// resolved protocol ids (skipping any name that didn't resolve so
/// the diagnostic from `resolve_bound_to_id` is the user-visible
/// outcome).
fn resolve_param_bounds(
    type_params: &[koja_ast::ast::TypeParam],
    scope: ResolutionScope<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Vec<GlobalRegistryId>> {
    type_params
        .iter()
        .map(|param| {
            param
                .bounds
                .iter()
                .filter_map(|bound| resolve_bound_to_id(bound, param.span, scope, diagnostics))
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
