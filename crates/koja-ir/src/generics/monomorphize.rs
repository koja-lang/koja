//! Per-instantiation specialization. Looks up the typecheck-layer
//! definition for the template, substitutes the discovered
//! [`ResolvedType`] args into each field / payload type / function
//! body, then lowers the substituted shape into a concrete
//! [`IRStructDecl`] / [`IREnumDecl`] / [`IRFunction`]. Newly-encountered
//! nested instantiations accumulate in `output.instantiations` for
//! [`super::instantiate`] to drain.

use std::collections::BTreeSet;

use koja_ast::identifier::{Identifier, ResolvedType};
use koja_typecheck::{
    EnumDefinition, FunctionSignature, GlobalKind, GlobalRegistry, ResolvedVariantData,
    StructDefinition,
};

use crate::enum_decl::{IREnumDecl, IREnumVariant, IRVariantPayload, IRVariantTag};
use crate::function::{IRFunction, IRSymbol};
use crate::lower::LowerOutput;
use crate::lower::package::{lower_function_inner, resolved_type_to_ir_type};
use crate::mangling::{mangled_function_name, mangled_method_name, mangled_type_name};
use crate::package::IRPackage;
use crate::struct_decl::{IRStructDecl, IRStructField};
use crate::types::IRType;

use super::substitute::{substitute_in_function, substitute_signature};
use super::{FunctionAstEntry, FunctionAstIndex, Instantiation, substitute_resolved_type};

/// Specialize one [`Instantiation`] into a concrete decl on the
/// owning [`IRPackage`]. Surfaces any nested instantiations
/// (and feature-gap diagnostics from re-lowered function bodies)
/// through `output` so the driver can chase them.
pub(super) fn monomorphize(
    inst: &Instantiation,
    registry: &GlobalRegistry,
    function_index: &FunctionAstIndex<'_>,
    packages: &mut [IRPackage],
    output: &mut LowerOutput,
) {
    let entry = registry.get(inst.template).unwrap_or_else(|| {
        panic!(
            "IR generics: instantiation template id `{}` missing from registry \
             (lower invariant violation)",
            inst.template,
        )
    });
    let owner_label = entry.identifier.package().to_string();
    let template_symbol = IRSymbol::from_identifier(&entry.identifier);
    let arg_types: Vec<IRType> = inst
        .args
        .iter()
        .map(|arg| resolved_type_to_ir_type(arg, registry, &mut output.instantiations))
        .collect();

    match &entry.kind {
        GlobalKind::Struct(Some(definition)) => {
            assert_arity(&entry.identifier, entry.type_params.len(), &inst.args);
            let symbol = mangled_type_name(&template_symbol, &arg_types);
            // `Global.CPtr<T>` is a primitive at the IR layer
            // ([`IRType::CPtr`]), not a struct decl, so its fields don't
            // exist as IR storage. Still enqueue its methods so call
            // sites against `CPtr_$T$.alloc`, `.free`, etc. resolve.
            if !is_primitive_struct_template(&entry.identifier) {
                let decl = monomorphize_struct(definition, inst, symbol, registry, output);
                owning_package(packages, &owner_label, &template_symbol)
                    .structs
                    .insert(decl.symbol.clone(), decl);
            }
            enqueue_member_methods(inst, registry, function_index, output);
        }
        GlobalKind::Enum(Some(definition)) => {
            assert_arity(&entry.identifier, entry.type_params.len(), &inst.args);
            let symbol = mangled_type_name(&template_symbol, &arg_types);
            let decl = monomorphize_enum(definition, inst, symbol, registry, output);
            owning_package(packages, &owner_label, &template_symbol)
                .enums
                .insert(decl.symbol.clone(), decl);
            enqueue_member_methods(inst, registry, function_index, output);
        }
        GlobalKind::Function(Some(signature)) => {
            // Three flavors share this arm:
            //
            // - Top-level generic function (`fn id<T>(x: T)`): `template ==
            //   owner`, `method_args` empty. Mangle args directly onto the
            //   function symbol.
            // - Method on a generic type, struct-level params only
            //   (`fn first(self) -> T` on `Pair<T, U>`): `template !=
            //   owner`, `method_args` empty. Mangle as `<struct>_$args$.<m>`.
            // - Method on a (possibly generic) type with its own type
            //   params (`fn map<U>` on `Option<T>`): `template != owner`,
            //   `method_args` non-empty. Mangle as `<struct>_$args$.<m>_$U$`.
            let method_arg_types: Vec<IRType> = inst
                .method_args
                .iter()
                .map(|arg| resolved_type_to_ir_type(arg, registry, &mut output.instantiations))
                .collect();
            let symbol = if inst.template == inst.owner {
                assert_arity(&entry.identifier, entry.type_params.len(), &inst.args);
                mangled_function_name(&template_symbol, &arg_types)
            } else {
                let owner_entry = registry.get(inst.owner).unwrap_or_else(|| {
                    panic!(
                        "IR generics: method template `{}` claims owner id `{}` \
                         which is missing from the registry",
                        entry.identifier, inst.owner,
                    )
                });
                assert_arity(
                    &owner_entry.identifier,
                    owner_entry.type_params.len(),
                    &inst.args,
                );
                assert_arity(
                    &entry.identifier,
                    entry.type_params.len(),
                    &inst.method_args,
                );
                let owner_symbol = IRSymbol::from_identifier(&owner_entry.identifier);
                mangled_method_name(
                    &owner_symbol,
                    &arg_types,
                    entry.identifier.last(),
                    &method_arg_types,
                )
            };
            let ast_entry = function_index.get(&inst.template).unwrap_or_else(|| {
                panic!(
                    "IR generics: function template `{}` registered but no AST \
                     entry in CheckedProgram (generics index invariant violation)",
                    entry.identifier,
                )
            });
            let Some(decl) = monomorphize_function(
                inst,
                ast_entry,
                &entry.identifier,
                signature,
                symbol,
                registry,
                output,
            ) else {
                return;
            };
            owning_package(packages, &owner_label, &template_symbol)
                .functions
                .insert(decl.symbol.clone(), decl);
        }
        other => panic!(
            "IR generics: instantiation template `{}` is a {}, but \
             only struct / enum / function templates can be monomorphized",
            entry.identifier,
            other.label(),
        ),
    }
}

fn monomorphize_struct(
    definition: &StructDefinition,
    inst: &Instantiation,
    symbol: IRSymbol,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> IRStructDecl {
    let mut fields = Vec::with_capacity(definition.fields.len());
    for (index, field) in definition.fields.iter().enumerate() {
        let substituted = substitute_resolved_type(&field.ty, &inst.args, inst.owner);
        let ir_type = resolved_type_to_ir_type(&substituted, registry, &mut output.instantiations);
        fields.push(IRStructField {
            index: index as u32,
            ir_type,
            name: field.name.clone(),
        });
    }
    IRStructDecl { fields, symbol }
}

fn monomorphize_enum(
    definition: &EnumDefinition,
    inst: &Instantiation,
    symbol: IRSymbol,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> IREnumDecl {
    let mut variants = Vec::with_capacity(definition.variants.len());
    for (index, variant) in definition.variants.iter().enumerate() {
        let payload = match &variant.data {
            ResolvedVariantData::Struct(fields) => {
                let mut ir_fields = Vec::with_capacity(fields.len());
                for (idx, field) in fields.iter().enumerate() {
                    let substituted = substitute_resolved_type(&field.ty, &inst.args, inst.owner);
                    ir_fields.push(IRStructField {
                        index: idx as u32,
                        ir_type: resolved_type_to_ir_type(
                            &substituted,
                            registry,
                            &mut output.instantiations,
                        ),
                        name: field.name.clone(),
                    });
                }
                IRVariantPayload::Struct(ir_fields)
            }
            ResolvedVariantData::Tuple(types) => {
                let translated = types
                    .iter()
                    .map(|ty| {
                        let substituted = substitute_resolved_type(ty, &inst.args, inst.owner);
                        resolved_type_to_ir_type(&substituted, registry, &mut output.instantiations)
                    })
                    .collect();
                IRVariantPayload::Tuple(translated)
            }
            ResolvedVariantData::Unit => IRVariantPayload::Unit,
        };
        variants.push(IREnumVariant {
            name: variant.name.clone(),
            payload,
            tag: IRVariantTag(index as u8),
        });
    }
    IREnumDecl { symbol, variants }
}

/// Substitute the function template's signature and body, then
/// re-lower fresh through [`lower_function_inner`] under the
/// monomorphized `symbol`. The body's `Expr.resolution` /
/// `Call.type_args` slots all carry concrete types after the walk,
/// so [`resolved_type_to_ir_type`] never sees a `TypeParam`.
///
/// Methods that declare their own type params get a second
/// substitution pass scoped at the method's own id so a `<U>` in
/// the body resolves to its concrete arg.
fn monomorphize_function(
    inst: &Instantiation,
    ast_entry: &FunctionAstEntry,
    identifier: &Identifier,
    signature: &FunctionSignature,
    symbol: IRSymbol,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Option<IRFunction> {
    let mut substituted_ast = ast_entry.function.clone();
    substitute_in_function(&mut substituted_ast, &inst.args, inst.owner);
    let mut substituted_signature = substitute_signature(signature, &inst.args, inst.owner);
    if !inst.method_args.is_empty() {
        substitute_in_function(&mut substituted_ast, &inst.method_args, inst.template);
        substituted_signature =
            substitute_signature(&substituted_signature, &inst.method_args, inst.template);
    }
    lower_function_inner(
        &substituted_ast,
        identifier,
        &substituted_signature,
        symbol,
        ast_entry.def_file,
        registry,
        output,
    )
}

/// Enqueue every method on `inst.template` (struct/enum) as a
/// function instantiation pinned to the same arg map. Methods inherit
/// their type-param scope from the enclosing type, so the resulting
/// `Instantiation` keeps `args` (the receiver's concrete type list)
/// but switches `template` to the method's id and points `owner` at
/// the type. Substitute calls in [`monomorphize_function`] then
/// resolve `Resolution::TypeParam { owner: type_id, .. }` references
/// inside the method body to the right concrete type.
///
/// Methods that declare their own type params (`fn map<U>` on
/// `Option<T>`) are skipped here, since they need a call site to pick
/// the method-level args and enqueue the full
/// `(template, args, method_args, owner)` quadruple. A struct
/// instantiation alone doesn't pin `<U>`.
fn enqueue_member_methods(
    inst: &Instantiation,
    registry: &GlobalRegistry,
    function_index: &FunctionAstIndex<'_>,
    output: &mut LowerOutput,
) {
    let entry = registry
        .get(inst.template)
        .expect("template id was just resolved");
    let owner_pkg = entry.identifier.package();
    let owner_path = entry.identifier.path();
    let protocol_method_names = protocol_method_names(&entry.kind, registry);
    for (id, candidate) in registry.iter_in_package(owner_pkg) {
        if !matches!(candidate.kind, GlobalKind::Function(Some(_))) {
            continue;
        }
        let path = candidate.identifier.path();
        if path.len() != owner_path.len() + 1 || !path.starts_with(owner_path) {
            continue;
        }
        if !function_index.contains_key(&id) {
            continue;
        }
        if !candidate.type_params.is_empty() {
            continue;
        }
        // Protocol-impl methods are call-site driven. Eagerly mono'ing
        // every time the struct is instantiated cascades into missing-
        // method errors when the impl body assumes a constraint the
        // concrete arg doesn't satisfy (e.g. `Debug for Pair<A, B>`
        // calling `A.format()` with no `A: Debug` bound, exploded by
        // a `Pair<Unit, X>` instance).
        if protocol_method_names.contains(candidate.identifier.last()) {
            continue;
        }
        output.instantiations.push(Instantiation {
            template: id,
            args: inst.args.clone(),
            method_args: Vec::new(),
            owner: inst.template,
        });
    }
}

/// Method names contributed by every protocol in `kind`'s
/// `conformances`. Drives the call-site-only filter in
/// [`enqueue_member_methods`].
fn protocol_method_names(kind: &GlobalKind, registry: &GlobalRegistry) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = BTreeSet::new();
    let conformances = match kind {
        GlobalKind::Struct(Some(def)) => &def.conformances,
        GlobalKind::Enum(Some(def)) => &def.conformances,
        _ => return names,
    };
    for protocol_id in conformances.keys() {
        let Some(protocol_entry) = registry.get(*protocol_id) else {
            continue;
        };
        let GlobalKind::Protocol(Some(protocol_def)) = &protocol_entry.kind else {
            continue;
        };
        for method in &protocol_def.methods {
            names.insert(method.name.clone());
        }
    }
    names
}

/// True for stdlib structs that lower to a primitive [`IRType`]:
/// `Global.CPtr<T>` (becomes `IRType::CPtr(...)`), `Global.List<T>`
/// (becomes `IRType::List(...)`), `Global.Map<K, V>` (becomes
/// `IRType::Map { ... }`), and `Global.Set<T>` (becomes
/// `IRType::Set(...)`). Mono enqueues the struct's methods (so
/// call sites can dispatch) but skips creating an `IRStructDecl`
/// (the IR has no struct storage to describe, as the pointer or hash-
/// table shape lives entirely inside the LLVM type lowering).
fn is_primitive_struct_template(identifier: &koja_ast::identifier::Identifier) -> bool {
    if identifier.package() != "Global" {
        return false;
    }
    let path = identifier.path();
    path.len() == 1 && matches!(path[0].as_str(), "CPtr" | "List" | "Map" | "Set")
}

fn assert_arity(
    identifier: &koja_ast::identifier::Identifier,
    expected: usize,
    args: &[ResolvedType],
) {
    assert_eq!(
        expected,
        args.len(),
        "IR generics: monomorphizing `{identifier}` requires {expected} type \
         args, got {}",
        args.len(),
    );
}

fn owning_package<'a>(
    packages: &'a mut [IRPackage],
    owner: &str,
    template: &IRSymbol,
) -> &'a mut IRPackage {
    packages
        .iter_mut()
        .find(|pkg| pkg.package == owner)
        .unwrap_or_else(|| {
            panic!(
                "IR generics: template `{template}` claims owner package \
                 `{owner}` but no IRPackage with that label exists",
            )
        })
}
