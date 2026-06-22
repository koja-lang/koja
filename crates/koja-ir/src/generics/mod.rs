//! Generic monomorphization for the IR pipeline.
//!
//! Generics are pipeline-internal scratch state: lowering produces
//! only concrete decls in [`crate::IRPackage`], and the typecheck
//! registry stays the single source of truth for generic-decl
//! shape. Discovery and specialization both live here.
//!
//! Three pieces:
//!
//! - [`Instantiation`] — one `(template, args, owner)` triple
//!   recorded at lowering time. Type instantiations come from
//!   [`crate::lower::package::resolved_type_to_ir_type`]; function
//!   instantiations come from [`crate::lower::expr::lower_call`];
//!   inline-method instantiations come from
//!   [`monomorphize::enqueue_member_methods`] when a generic
//!   struct/enum is mono'd.
//! - [`instantiate`] — the worklist driver. Dedupes by
//!   [`BTreeSet`] and dispatches each `(template, args, owner)`
//!   triple to the right [`monomorphize`] arm: struct, enum, or
//!   function. Each arm substitutes the discovered args into the
//!   template's typecheck definition / AST body via
//!   [`substitute_resolved_type`] (a local copy — the typecheck
//!   variant lives in a different layer with a different
//!   invariant about Phantom slots), then re-lowers the
//!   substituted shape into a concrete [`crate::IRStructDecl`] /
//!   [`crate::IREnumDecl`] / [`crate::IRFunction`].
//! - [`FunctionAstIndex`] — registry-id to AST-`Function`-borrow
//!   map built from `CheckedPackage`s once per `instantiate` call.
//!   Function bodies live in the AST, not the registry.
//!
//! The loop is a worklist because each arm may surface fresh
//! instantiations (a generic field whose type is a non-leaf
//! generic; a generic call inside a substituted function body).
//! The driver pops, mono'izes, drains discoveries back onto the
//! worklist, and stops when the set stabilizes.

mod monomorphize;
mod substitute;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use koja_ast::ast::{Function, ImplMember, Item};
use koja_ast::identifier::{AnonymousKind, GlobalRegistryId, Identifier, Resolution, ResolvedType};
use koja_typecheck::{CheckedPackage, GlobalRegistry};

use crate::lower::LowerOutput;
use crate::lower::package::{extend_target_path, impl_target_name};
use crate::package::IRPackage;

/// One discovered generic instantiation. Recorded by
/// [`crate::lower::package::resolved_type_to_ir_type`] every time it
/// lowers a [`ResolvedType`] with a non-empty `type_args` list, and
/// by [`monomorphize::monomorphize`] every time it enqueues a
/// generic struct/enum's inline methods. Deduped by [`instantiate`]
/// via [`BTreeSet`] before any monomorphization runs.
///
/// `owner` is the [`GlobalRegistryId`] that owns the type params
/// `args` substitutes for. For struct/enum templates (and top-level
/// generic functions) it equals `template`. For inline methods on
/// generic types it points at the *enclosing* type — that's where
/// the body's `Resolution::TypeParam { owner, .. }` references
/// come from (lift gave the method an inherited scope, not its
/// own).
///
/// `method_args` carries the method's own type-args when the
/// template is a method that declares its own type parameters
/// (e.g. `fn map<U>` on `Option<T>`). Empty for everything else —
/// struct/enum templates, top-level generic functions, and methods
/// that only use their enclosing type's params. When non-empty,
/// monomorphization substitutes the method body twice: once with
/// `(args, owner)` for the receiver's params and once with
/// `(method_args, template)` for the method's own params.
#[derive(Debug, Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct Instantiation {
    pub(crate) template: GlobalRegistryId,
    pub(crate) args: Vec<ResolvedType>,
    pub(crate) method_args: Vec<ResolvedType>,
    pub(crate) owner: GlobalRegistryId,
}

/// Dedupe `instantiations`, monomorphize each one against the
/// typecheck `registry` and `checked_packages` (function bodies
/// live in the AST, not the registry), and insert the resulting
/// concrete decl into the [`IRPackage`] in `packages` whose
/// `package` label matches the template's owning package. Loops
/// until the instantiation set stabilizes — a single round may
/// surface fresh instantiations (a generic field whose declared
/// type is itself a non-leaf generic instantiation, or a generic
/// call inside a substituted body).
///
/// Diagnostics surfaced while re-lowering substituted bodies push
/// to `output.diagnostics`; nested instantiations push back onto
/// the worklist. Caller is responsible for short-circuiting on a
/// non-empty `output.diagnostics` after this returns.
///
/// Panics if an instantiation references a registry id with no
/// lifted definition or if no [`IRPackage`] in `packages` matches
/// the template's owner — both are lower invariant violations.
pub(crate) fn instantiate(
    instantiations: Vec<Instantiation>,
    registry: &GlobalRegistry,
    checked_packages: &[CheckedPackage],
    packages: &mut [IRPackage],
    output: &mut LowerOutput,
) {
    let function_index = build_function_index(checked_packages, registry);
    let mut done: BTreeSet<Instantiation> = BTreeSet::new();
    let mut worklist: Vec<Instantiation> = instantiations;
    while let Some(inst) = worklist.pop() {
        if !done.insert(inst.clone()) {
            continue;
        }
        monomorphize::monomorphize(&inst, registry, &function_index, packages, output);
        worklist.append(&mut std::mem::take(&mut output.instantiations));
        // Mono'd bodies can mint closures / spawn wrappers via
        // `lower_function_inner`. `lower_package_inner` drains those
        // once at the end of each initial-pass lowering, before mono
        // runs, so anything pushed here would otherwise be stranded.
        drain_synthesized_into_packages(packages, output);
    }
}

/// Route each synthesized function to the `IRPackage` whose label
/// matches its symbol's package prefix; fall back to the first
/// package on a misalignment so a missing target surfaces as a seal
/// panic instead of a silent drop.
fn drain_synthesized_into_packages(packages: &mut [IRPackage], output: &mut LowerOutput) {
    for synthesized in output.synthesized_functions.drain(..) {
        let symbol_str = synthesized.symbol.mangled();
        let pkg_prefix = symbol_str.split('.').next().unwrap_or(symbol_str);
        let index = packages
            .iter()
            .position(|pkg| pkg.package == pkg_prefix)
            .unwrap_or(0);
        let owner = packages
            .get_mut(index)
            .expect("IR generics: no IRPackage available to host synthesized function");
        owner
            .functions
            .insert(synthesized.symbol.clone(), synthesized);
    }
}

/// Map every function template's `GlobalRegistryId` to its AST
/// node. Covers top-level `fn`s, inline methods on struct/enum
/// decls, and methods in `impl` / `extend` blocks (the latter may
/// target another package).
/// An indexed function template: the AST node plus the path of the
/// file it was parsed from. The path threads through to
/// [`crate::lower::package::lower_function_inner`] when a
/// monomorphization re-lowers the body, so specialized generics keep
/// their source attribution for DWARF.
pub(super) struct FunctionAstEntry<'a> {
    pub def_file: Option<&'a Path>,
    pub function: &'a Function,
}

pub(super) type FunctionAstIndex<'a> = BTreeMap<GlobalRegistryId, FunctionAstEntry<'a>>;

fn build_function_index<'a>(
    packages: &'a [CheckedPackage],
    registry: &GlobalRegistry,
) -> FunctionAstIndex<'a> {
    let mut map: FunctionAstIndex<'a> = BTreeMap::new();
    for pkg in packages {
        for file in &pkg.files {
            let def_file = file.path.as_deref();
            for item in &file.items {
                index_item(item, &pkg.package, def_file, registry, &mut map);
            }
        }
    }
    map
}

fn index_item<'a>(
    item: &'a Item,
    package: &str,
    def_file: Option<&'a Path>,
    registry: &GlobalRegistry,
    map: &mut FunctionAstIndex<'a>,
) {
    match item {
        Item::Function(function) => {
            let identifier = Identifier::new(package, vec![function.name.clone()]);
            insert_function(map, registry, &identifier, function, def_file);
        }
        Item::Struct(decl) => {
            for function in &decl.functions {
                let identifier =
                    Identifier::new(package, vec![decl.name.clone(), function.name.clone()]);
                insert_function(map, registry, &identifier, function, def_file);
            }
        }
        Item::Enum(decl) => {
            for function in &decl.functions {
                let identifier =
                    Identifier::new(package, vec![decl.name.clone(), function.name.clone()]);
                insert_function(map, registry, &identifier, function, def_file);
            }
        }
        Item::Impl(impl_block) => {
            let Some(target_name) = impl_target_name(&impl_block.target) else {
                return;
            };
            for member in &impl_block.members {
                let ImplMember::Function(function) = member else {
                    continue;
                };
                let identifier = Identifier::new(
                    package,
                    vec![target_name.to_string(), function.name.clone()],
                );
                insert_function(map, registry, &identifier, function, def_file);
            }
        }
        Item::Extend(extend_block) => {
            let Some((target_package, target_name)) =
                extend_target_path(&extend_block.target, package)
            else {
                return;
            };
            for member in &extend_block.members {
                let ImplMember::Function(function) = member else {
                    continue;
                };
                let identifier = Identifier::new(
                    &target_package,
                    vec![target_name.to_string(), function.name.clone()],
                );
                insert_function(map, registry, &identifier, function, def_file);
            }
        }
        _ => {}
    }
}

fn insert_function<'a>(
    map: &mut FunctionAstIndex<'a>,
    registry: &GlobalRegistry,
    identifier: &Identifier,
    function: &'a Function,
    def_file: Option<&'a Path>,
) {
    if let Some((id, _)) = registry.lookup(identifier) {
        map.insert(id, FunctionAstEntry { def_file, function });
    }
}

/// Substitute `args` into `template`, replacing every leaf
/// [`Resolution::TypeParam { owner, .. }`][Resolution::TypeParam]
/// whose `owner` matches with `args[index]`. Other heads recurse
/// into their `type_args`.
///
/// Local copy of the typecheck-side substitution — the contract
/// here is "every Param leaf has a concrete arg" (enforced by the
/// inference-then-substitute flow at construction sites in
/// typecheck), so we panic on out-of-range index instead of
/// substituting to `ResolvedType::unresolved` like the typecheck
/// variant does for unresolved Phantom slots.
pub(crate) fn substitute_resolved_type(
    template: &ResolvedType,
    args: &[ResolvedType],
    owner: GlobalRegistryId,
) -> ResolvedType {
    match template {
        ResolvedType::Named {
            resolution:
                Resolution::TypeParam {
                    owner: param_owner,
                    index,
                },
            ..
        } if *param_owner == owner => {
            args.get(index.as_u32() as usize)
                .cloned()
                .unwrap_or_else(|| {
                    panic!(
                        "IR generics: TypeParam index {} out of range \
                     (owner `{owner}`, args.len() == {})",
                        index.as_u32(),
                        args.len(),
                    );
                })
        }
        ResolvedType::Named {
            resolution,
            type_args,
        } => ResolvedType::Named {
            resolution: *resolution,
            type_args: type_args
                .iter()
                .map(|arg| substitute_resolved_type(arg, args, owner))
                .collect(),
        },
        ResolvedType::Anonymous(AnonymousKind::Function { params, ret }) => {
            ResolvedType::Anonymous(AnonymousKind::Function {
                params: params
                    .iter()
                    .map(|p| substitute_resolved_type(p, args, owner))
                    .collect(),
                ret: Box::new(substitute_resolved_type(ret, args, owner)),
            })
        }
        ResolvedType::Union(members) => ResolvedType::Union(
            members
                .iter()
                .map(|m| substitute_resolved_type(m, args, owner))
                .collect(),
        ),
        ResolvedType::Unresolved => ResolvedType::Unresolved,
    }
}
