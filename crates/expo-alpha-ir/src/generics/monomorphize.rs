//! Per-instantiation specialization. Looks up the typecheck-layer
//! definition for the template, substitutes the discovered
//! [`ResolvedType`] args into each field / payload type / function
//! body, then lowers the substituted shape into a concrete
//! [`IRStructDecl`] / [`IREnumDecl`] / [`IRFunction`]. Newly-encountered
//! nested instantiations accumulate in `output.instantiations` for
//! [`super::instantiate`] to drain.

use expo_alpha_typecheck::{
    EnumDefinition, FunctionSignature, GlobalKind, GlobalRegistry, ResolvedVariantData,
    StructDefinition,
};
use expo_ast::ast::Function;
use expo_ast::identifier::{Identifier, ResolvedType};

use crate::enum_decl::{IREnumDecl, IREnumVariant, IRVariantPayload, IRVariantTag};
use crate::function::{IRFunction, IRSymbol};
use crate::lower::LowerOutput;
use crate::lower::package::{lower_function_inner, resolved_type_to_ir_type};
use crate::mangling::mangled_function_name;
use crate::mangling::mangled_type_name;
use crate::package::IRPackage;
use crate::struct_decl::{IRStructDecl, IRStructField};
use crate::types::IRType;

use super::substitute::{substitute_in_function, substitute_signature};
use super::{FunctionAstIndex, Instantiation, substitute_resolved_type};

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
            "alpha IR generics: instantiation template id `{}` missing from registry — \
             lower invariant violation",
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
            let decl = monomorphize_struct(definition, inst, symbol, registry, output);
            owning_package(packages, &owner_label, &template_symbol)
                .structs
                .insert(decl.symbol.clone(), decl);
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
            // Methods on generic types inherit the type's params; mangle as
            // `<struct>_$args$.<method>` so the symbol matches what
            // `lower_method_call` synthesizes at every call site. Top-level
            // generic functions mangle their args directly onto the function
            // symbol via [`mangled_function_name`].
            let symbol = if inst.template == inst.owner {
                assert_arity(&entry.identifier, entry.type_params.len(), &inst.args);
                mangled_function_name(&template_symbol, &arg_types)
            } else {
                let owner_entry = registry.get(inst.owner).unwrap_or_else(|| {
                    panic!(
                        "alpha IR generics: method template `{}` claims owner id `{}` \
                         which is missing from the registry",
                        entry.identifier, inst.owner,
                    )
                });
                assert_arity(
                    &owner_entry.identifier,
                    owner_entry.type_params.len(),
                    &inst.args,
                );
                let owner_symbol = IRSymbol::from_identifier(&owner_entry.identifier);
                let mangled_owner = mangled_type_name(&owner_symbol, &arg_types);
                mangled_owner.derived(&format!(".{}", entry.identifier.last()))
            };
            let function_ast = function_index.get(&inst.template).unwrap_or_else(|| {
                panic!(
                    "alpha IR generics: function template `{}` registered but no AST \
                     entry in CheckedProgram — generics index invariant violation",
                    entry.identifier,
                )
            });
            let Some(decl) = monomorphize_function(
                inst,
                function_ast,
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
            "alpha IR generics: instantiation template `{}` is a {} — \
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
fn monomorphize_function(
    inst: &Instantiation,
    function_ast: &Function,
    identifier: &Identifier,
    signature: &FunctionSignature,
    symbol: IRSymbol,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Option<IRFunction> {
    let mut substituted_ast = function_ast.clone();
    substitute_in_function(&mut substituted_ast, &inst.args, inst.owner);
    let substituted_signature = substitute_signature(signature, &inst.args, inst.owner);
    lower_function_inner(
        &substituted_ast,
        identifier,
        &substituted_signature,
        symbol,
        registry,
        output,
    )
}

/// Enqueue every method on `inst.template` (struct/enum) as a
/// function instantiation pinned to the same arg map. Methods inherit
/// their type-param scope from the enclosing type, so the resulting
/// `Instantiation` keeps `args` (the receiver's concrete type list)
/// but switches `template` to the method's id and points `owner` at
/// the type — substitute calls in [`monomorphize_function`] then
/// resolve `Resolution::TypeParam { owner: type_id, .. }` references
/// inside the method body to the right concrete type.
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
        output.instantiations.push(Instantiation {
            template: id,
            args: inst.args.clone(),
            owner: inst.template,
        });
    }
}

fn assert_arity(
    identifier: &expo_ast::identifier::Identifier,
    expected: usize,
    args: &[ResolvedType],
) {
    assert_eq!(
        expected,
        args.len(),
        "alpha IR generics: monomorphizing `{identifier}` requires {expected} type \
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
                "alpha IR generics: template `{template}` claims owner package \
                 `{owner}` but no IRPackage with that label exists",
            )
        })
}
