//! Enum-shaped seal invariants.
//!
//! Mirrors [`super::structs`] — two complementary checks:
//!
//! - [`seal_enum_decls`] runs per package and validates every
//!   [`IREnumDecl`] in isolation (key matches symbol, dense
//!   declaration-ordered tags, unique variant names, variant count
//!   fits in a `u8`, payload-supported types). Struct-variant
//!   payloads delegate to
//!   [`super::structs::seal_named_field_layout`] so the dense-index
//!   / unique-name / supported-type rules live in one place shared
//!   with [`crate::IRStructDecl`].
//! - [`seal_enum_ops`] runs across the assembled program / script
//!   (call site supplies the cross-package enum lookup) and
//!   validates every [`IRInstruction::EnumConstruct`]: target enum
//!   is registered, `tag.0 < variants.len()`, and `payload`'s shape
//!   matches the variant's declared [`IRVariantPayload`] (Unit↔Unit,
//!   Tuple arity match, Struct len + canonicalization match).
//!
//! Both checks panic on violation through [`super::seal_panic`] —
//! enum seal failures indicate a lower / lift_signatures bug, not a
//! user error.

use std::collections::HashSet;

use crate::enum_decl::{EnumPayloadInit, IREnumDecl, IRVariantPayload};
use crate::function::{IRInstruction, IRSymbol};
use crate::package::IRPackage;

use super::seal_panic;
use super::structs::seal_named_field_layout;

pub(super) fn seal_enum_decls(pkg: &IRPackage) {
    for (sym, decl) in &pkg.enums {
        if sym != &decl.symbol {
            seal_panic(&format!(
                "package `{}` keys enum at `{sym}` but the enum's own symbol is `{}`",
                pkg.package, decl.symbol,
            ));
        }
        seal_enum_decl(decl);
    }
}

fn seal_enum_decl(decl: &IREnumDecl) {
    let owner = format!("enum `{}`", decl.symbol);
    if decl.variants.len() > usize::from(u8::MAX) + 1 {
        seal_panic(&format!(
            "{owner} declares {n} variants; the IR caps the count at {max} \
             (transient — the LLVM `i8` tag width)",
            n = decl.variants.len(),
            max = u8::MAX as usize + 1,
        ));
    }
    let mut seen_names: HashSet<&str> = HashSet::with_capacity(decl.variants.len());
    for (position, variant) in decl.variants.iter().enumerate() {
        if usize::from(variant.tag.0) != position {
            seal_panic(&format!(
                "{owner} variant `{name}` has tag {tag} at position {position}; \
                 tags must be dense in declaration order (0..n)",
                name = variant.name,
                tag = variant.tag.0,
            ));
        }
        if !seen_names.insert(variant.name.as_str()) {
            seal_panic(&format!(
                "{owner} declares duplicate variant name `{}`",
                variant.name,
            ));
        }
        seal_variant_payload(&owner, variant);
    }
}

fn seal_variant_payload(owner: &str, variant: &crate::enum_decl::IREnumVariant) {
    let variant_label = format!("{owner} variant `{}`", variant.name);
    match &variant.payload {
        IRVariantPayload::Struct(fields) => {
            seal_named_field_layout(&variant_label, fields);
        }
        IRVariantPayload::Tuple(types) => {
            for (index, ty) in types.iter().enumerate() {
                super::require_supported_type(ty, &|| {
                    format!("{variant_label} tuple element #{index}")
                });
            }
        }
        IRVariantPayload::Unit => {}
    }
}

/// Cross-instruction enum check. Driven by the `(owner, inst)`
/// stream the caller produces (see
/// [`super::structs::package_instructions`] /
/// [`super::structs::function_instructions`]); `lookup` resolves an
/// [`IRSymbol::mangled`] view to the registered [`IREnumDecl`]
/// (`IRProgram::enum_decl` / `IRScript::enum_decl`).
pub(super) fn seal_enum_ops<'inst, 'decl>(
    instructions: impl IntoIterator<Item = (String, &'inst IRInstruction)>,
    lookup: &impl Fn(&str) -> Option<&'decl IREnumDecl>,
) {
    for (owner, inst) in instructions {
        let IRInstruction::EnumConstruct {
            dest: _,
            payload,
            tag,
            ty,
        } = inst
        else {
            continue;
        };
        let decl = require_enum(lookup, ty, &owner);
        let Some(variant) = decl.variants.get(usize::from(tag.0)) else {
            seal_panic(&format!(
                "{owner}: EnumConstruct on `{ty}` references tag {tag} but the decl only \
                 declares {count} variant(s)",
                count = decl.variants.len(),
            ));
        };
        match (&variant.payload, payload) {
            (IRVariantPayload::Unit, EnumPayloadInit::Unit) => {}
            (IRVariantPayload::Tuple(declared), EnumPayloadInit::Tuple(values)) => {
                if values.len() != declared.len() {
                    seal_panic(&format!(
                        "{owner}: EnumConstruct for `{ty}.{name}` carries {got} tuple value(s) \
                         but the variant declares {expected}",
                        name = variant.name,
                        got = values.len(),
                        expected = declared.len(),
                    ));
                }
            }
            (IRVariantPayload::Struct(declared), EnumPayloadInit::Struct(inits)) => {
                if inits.len() != declared.len() {
                    seal_panic(&format!(
                        "{owner}: EnumConstruct for `{ty}.{name}` carries {got} struct field(s) \
                         but the variant declares {expected}",
                        name = variant.name,
                        got = inits.len(),
                        expected = declared.len(),
                    ));
                }
                for (position, init) in inits.iter().enumerate() {
                    if init.index as usize != position {
                        seal_panic(&format!(
                            "{owner}: EnumConstruct for `{ty}.{name}` field-init #{position} \
                             has index {index}; field inits must be canonicalized to \
                             declaration order",
                            name = variant.name,
                            index = init.index,
                        ));
                    }
                }
            }
            (declared, supplied) => {
                seal_panic(&format!(
                    "{owner}: EnumConstruct for `{ty}.{name}` payload shape mismatch \
                     (declared {declared}, supplied {supplied})",
                    name = variant.name,
                    declared = payload_shape_label(declared),
                    supplied = init_shape_label(supplied),
                ));
            }
        }
    }
}

fn payload_shape_label(payload: &IRVariantPayload) -> &'static str {
    match payload {
        IRVariantPayload::Struct(_) => "Struct",
        IRVariantPayload::Tuple(_) => "Tuple",
        IRVariantPayload::Unit => "Unit",
    }
}

fn init_shape_label(init: &EnumPayloadInit) -> &'static str {
    match init {
        EnumPayloadInit::Struct(_) => "Struct",
        EnumPayloadInit::Tuple(_) => "Tuple",
        EnumPayloadInit::Unit => "Unit",
    }
}

fn require_enum<'decl>(
    lookup: &impl Fn(&str) -> Option<&'decl IREnumDecl>,
    symbol: &IRSymbol,
    owner: &str,
) -> &'decl IREnumDecl {
    lookup(symbol.mangled()).unwrap_or_else(|| {
        seal_panic(&format!(
            "{owner}: enum symbol `{symbol}` is not registered in any package",
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use expo_ast::identifier::Identifier;

    use crate::enum_decl::{
        EnumPayloadInit, IREnumDecl, IREnumVariant, IRVariantPayload, IRVariantTag,
    };
    use crate::function::{IRInstruction, IRSymbol};
    use crate::package::IRPackage;
    use crate::struct_decl::{IRStructField, StructFieldInit};
    use crate::types::{IRType, ValueId};

    use super::{seal_enum_decls, seal_enum_ops};

    fn symbol(name: &str) -> IRSymbol {
        IRSymbol::from_identifier(&Identifier::new("TestApp", vec![name.to_string()]))
    }

    fn option_decl() -> IREnumDecl {
        IREnumDecl {
            symbol: symbol("Option"),
            variants: vec![
                IREnumVariant {
                    name: "None".to_string(),
                    payload: IRVariantPayload::Unit,
                    tag: IRVariantTag(0),
                },
                IREnumVariant {
                    name: "Some".to_string(),
                    payload: IRVariantPayload::Tuple(vec![IRType::Int64]),
                    tag: IRVariantTag(1),
                },
            ],
        }
    }

    fn package_with(decl: IREnumDecl) -> IRPackage {
        let mut enums = BTreeMap::new();
        enums.insert(decl.symbol.clone(), decl);
        IRPackage {
            enums,
            functions: BTreeMap::new(),
            package: "TestApp".to_string(),
            structs: BTreeMap::new(),
        }
    }

    fn lookup_against<'a>(decls: &'a [IREnumDecl]) -> impl Fn(&str) -> Option<&'a IREnumDecl> + 'a {
        move |needle: &str| decls.iter().find(|decl| decl.symbol.mangled() == needle)
    }

    #[test]
    fn well_formed_decls_pass_seal() {
        let pkg = package_with(option_decl());
        seal_enum_decls(&pkg);
    }

    #[test]
    #[should_panic(expected = "tags must be dense in declaration order")]
    fn non_dense_tag_panics() {
        let mut decl = option_decl();
        decl.variants[1].tag = IRVariantTag(7);
        seal_enum_decls(&package_with(decl));
    }

    #[test]
    #[should_panic(expected = "declares duplicate variant name")]
    fn duplicate_variant_name_panics() {
        let mut decl = option_decl();
        decl.variants[1].name = "None".to_string();
        seal_enum_decls(&package_with(decl));
    }

    #[test]
    #[should_panic(expected = "package `TestApp` keys enum at")]
    fn key_symbol_mismatch_panics() {
        let decl = option_decl();
        let mut enums = BTreeMap::new();
        enums.insert(symbol("Misnamed"), decl);
        let pkg = IRPackage {
            enums,
            functions: BTreeMap::new(),
            package: "TestApp".to_string(),
            structs: BTreeMap::new(),
        };
        seal_enum_decls(&pkg);
    }

    #[test]
    fn well_formed_enum_construct_passes_seal() {
        let decl = option_decl();
        let inst = IRInstruction::EnumConstruct {
            dest: ValueId(0),
            payload: EnumPayloadInit::Tuple(vec![ValueId(1)]),
            tag: IRVariantTag(1),
            ty: decl.symbol.clone(),
        };
        let decls = vec![decl];
        let lookup = lookup_against(&decls);
        seal_enum_ops(std::iter::once(("test".to_string(), &inst)), &lookup);
    }

    #[test]
    #[should_panic(expected = "is not registered in any package")]
    fn enum_construct_with_unregistered_symbol_panics() {
        let inst = IRInstruction::EnumConstruct {
            dest: ValueId(0),
            payload: EnumPayloadInit::Unit,
            tag: IRVariantTag(0),
            ty: symbol("Unknown"),
        };
        let decls: Vec<IREnumDecl> = vec![];
        let lookup = lookup_against(&decls);
        seal_enum_ops(std::iter::once(("test".to_string(), &inst)), &lookup);
    }

    #[test]
    #[should_panic(expected = "references tag #5 but the decl only declares 2 variant(s)")]
    fn enum_construct_out_of_range_tag_panics() {
        let decl = option_decl();
        let inst = IRInstruction::EnumConstruct {
            dest: ValueId(0),
            payload: EnumPayloadInit::Unit,
            tag: IRVariantTag(5),
            ty: decl.symbol.clone(),
        };
        let decls = vec![decl];
        let lookup = lookup_against(&decls);
        seal_enum_ops(std::iter::once(("test".to_string(), &inst)), &lookup);
    }

    #[test]
    #[should_panic(expected = "payload shape mismatch (declared Unit, supplied Tuple)")]
    fn enum_construct_payload_shape_mismatch_panics() {
        let decl = option_decl();
        let inst = IRInstruction::EnumConstruct {
            dest: ValueId(0),
            payload: EnumPayloadInit::Tuple(vec![ValueId(1)]),
            tag: IRVariantTag(0),
            ty: decl.symbol.clone(),
        };
        let decls = vec![decl];
        let lookup = lookup_against(&decls);
        seal_enum_ops(std::iter::once(("test".to_string(), &inst)), &lookup);
    }

    #[test]
    #[should_panic(expected = "carries 2 tuple value(s) but the variant declares 1")]
    fn enum_construct_tuple_arity_mismatch_panics() {
        let decl = option_decl();
        let inst = IRInstruction::EnumConstruct {
            dest: ValueId(0),
            payload: EnumPayloadInit::Tuple(vec![ValueId(1), ValueId(2)]),
            tag: IRVariantTag(1),
            ty: decl.symbol.clone(),
        };
        let decls = vec![decl];
        let lookup = lookup_against(&decls);
        seal_enum_ops(std::iter::once(("test".to_string(), &inst)), &lookup);
    }

    #[test]
    #[should_panic(expected = "field inits must be canonicalized to declaration order")]
    fn enum_construct_struct_init_order_violation_panics() {
        let decl = IREnumDecl {
            symbol: symbol("Shape"),
            variants: vec![IREnumVariant {
                name: "Rect".to_string(),
                payload: IRVariantPayload::Struct(vec![
                    IRStructField {
                        index: 0,
                        ir_type: IRType::Int64,
                        name: "w".to_string(),
                    },
                    IRStructField {
                        index: 1,
                        ir_type: IRType::Int64,
                        name: "h".to_string(),
                    },
                ]),
                tag: IRVariantTag(0),
            }],
        };
        let inst = IRInstruction::EnumConstruct {
            dest: ValueId(0),
            payload: EnumPayloadInit::Struct(vec![
                StructFieldInit {
                    index: 1,
                    value: ValueId(1),
                },
                StructFieldInit {
                    index: 0,
                    value: ValueId(2),
                },
            ]),
            tag: IRVariantTag(0),
            ty: decl.symbol.clone(),
        };
        let decls = vec![decl];
        let lookup = lookup_against(&decls);
        seal_enum_ops(std::iter::once(("test".to_string(), &inst)), &lookup);
    }
}
