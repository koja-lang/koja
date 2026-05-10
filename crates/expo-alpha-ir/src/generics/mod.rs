//! Generic monomorphization for the alpha IR pipeline.
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

use expo_alpha_typecheck::{CheckedPackage, GlobalRegistry};
use expo_ast::ast::{Function, ImplMember, Item};
use expo_ast::identifier::{
    AnonymousKind, FnParam, GlobalRegistryId, Identifier, Resolution, ResolvedType,
};

use crate::lower::LowerOutput;
use crate::lower::package::impl_target_name;
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
    }
}

/// Map every function template's `GlobalRegistryId` to its AST
/// node. Built once per [`instantiate`] call; covers top-level
/// `fn`s, inline `fn` items on struct/enum decls, and methods in
/// `impl` blocks. Skipped for v1-only [`Item`] kinds (constants,
/// imports) — alpha doesn't lower them.
pub(super) type FunctionAstIndex<'a> = BTreeMap<GlobalRegistryId, &'a Function>;

fn build_function_index<'a>(
    packages: &'a [CheckedPackage],
    registry: &GlobalRegistry,
) -> FunctionAstIndex<'a> {
    let mut map: FunctionAstIndex<'a> = BTreeMap::new();
    for pkg in packages {
        for file in &pkg.files {
            for item in &file.items {
                index_item(item, &pkg.package, registry, &mut map);
            }
        }
    }
    map
}

fn index_item<'a>(
    item: &'a Item,
    package: &str,
    registry: &GlobalRegistry,
    map: &mut FunctionAstIndex<'a>,
) {
    match item {
        Item::Function(function) => {
            let identifier = Identifier::new(package, vec![function.name.clone()]);
            insert_function(map, registry, &identifier, function);
        }
        Item::Struct(decl) => {
            for function in &decl.functions {
                let identifier =
                    Identifier::new(package, vec![decl.name.clone(), function.name.clone()]);
                insert_function(map, registry, &identifier, function);
            }
        }
        Item::Enum(decl) => {
            for function in &decl.functions {
                let identifier =
                    Identifier::new(package, vec![decl.name.clone(), function.name.clone()]);
                insert_function(map, registry, &identifier, function);
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
                insert_function(map, registry, &identifier, function);
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
) {
    if let Some((id, _)) = registry.lookup(identifier) {
        map.insert(id, function);
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
                        "alpha IR generics: TypeParam index {} out of range \
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
                    .map(|p| FnParam {
                        mode: p.mode,
                        ty: substitute_resolved_type(&p.ty, args, owner),
                    })
                    .collect(),
                ret: Box::new(substitute_resolved_type(ret, args, owner)),
            })
        }
        ResolvedType::Unresolved => ResolvedType::Unresolved,
    }
}
