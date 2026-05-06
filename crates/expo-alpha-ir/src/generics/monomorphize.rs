//! Per-instantiation specialization. Looks up the typecheck-layer
//! definition for the template, substitutes the discovered
//! [`ResolvedType`] args into each field / payload type, then
//! lowers the substituted shape into a concrete [`IRStructDecl`] /
//! [`IREnumDecl`]. Newly-encountered nested instantiations
//! accumulate in `discovered` for [`super::instantiate`] to drain.

use expo_alpha_typecheck::{
    EnumDefinition, GlobalKind, GlobalRegistry, ResolvedVariantData, StructDefinition,
};
use expo_ast::identifier::ResolvedType;

use crate::enum_decl::{IREnumDecl, IREnumVariant, IRVariantPayload, IRVariantTag};
use crate::function::IRSymbol;
use crate::lower::package::resolved_type_to_ir_type;
use crate::mangling::mangled_type_name;
use crate::package::IRPackage;
use crate::struct_decl::{IRStructDecl, IRStructField};
use crate::types::IRType;

use super::{Instantiation, substitute_resolved_type};

/// Specialize one [`Instantiation`] into a concrete decl on the
/// owning [`IRPackage`]. Surfaces any nested instantiations
/// encountered while lowering substituted field / payload types
/// through `discovered` so the driver can chase them.
pub(super) fn monomorphize(
    inst: &Instantiation,
    registry: &GlobalRegistry,
    packages: &mut [IRPackage],
    discovered: &mut Vec<Instantiation>,
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
        .map(|arg| resolved_type_to_ir_type(arg, registry, discovered))
        .collect();
    let symbol = mangled_type_name(&template_symbol, &arg_types);

    match &entry.kind {
        GlobalKind::Struct(Some(definition)) => {
            assert_arity(&entry.identifier, definition.type_params.len(), &inst.args);
            let decl = monomorphize_struct(definition, inst, symbol, registry, discovered);
            owning_package(packages, &owner_label, &template_symbol)
                .structs
                .insert(decl.symbol.clone(), decl);
        }
        GlobalKind::Enum(Some(definition)) => {
            assert_arity(&entry.identifier, definition.type_params.len(), &inst.args);
            let decl = monomorphize_enum(definition, inst, symbol, registry, discovered);
            owning_package(packages, &owner_label, &template_symbol)
                .enums
                .insert(decl.symbol.clone(), decl);
        }
        other => panic!(
            "alpha IR generics: instantiation template `{}` is a {} — \
             only struct / enum templates can be monomorphized",
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
    discovered: &mut Vec<Instantiation>,
) -> IRStructDecl {
    let mut fields = Vec::with_capacity(definition.fields.len());
    for (index, field) in definition.fields.iter().enumerate() {
        let substituted = substitute_resolved_type(&field.ty, &inst.args, inst.template);
        let ir_type = resolved_type_to_ir_type(&substituted, registry, discovered);
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
    discovered: &mut Vec<Instantiation>,
) -> IREnumDecl {
    let mut variants = Vec::with_capacity(definition.variants.len());
    for (index, variant) in definition.variants.iter().enumerate() {
        let payload = match &variant.data {
            ResolvedVariantData::Struct(fields) => {
                let mut ir_fields = Vec::with_capacity(fields.len());
                for (idx, field) in fields.iter().enumerate() {
                    let substituted =
                        substitute_resolved_type(&field.ty, &inst.args, inst.template);
                    ir_fields.push(IRStructField {
                        index: idx as u32,
                        ir_type: resolved_type_to_ir_type(&substituted, registry, discovered),
                        name: field.name.clone(),
                    });
                }
                IRVariantPayload::Struct(ir_fields)
            }
            ResolvedVariantData::Tuple(types) => {
                let translated = types
                    .iter()
                    .map(|ty| {
                        let substituted = substitute_resolved_type(ty, &inst.args, inst.template);
                        resolved_type_to_ir_type(&substituted, registry, discovered)
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
