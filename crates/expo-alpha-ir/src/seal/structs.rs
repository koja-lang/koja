//! Struct-shaped seal invariants.
//!
//! Two complementary checks:
//!
//! - [`seal_struct_decls`] runs per package and validates every
//!   [`IRStructDecl`] in isolation — keys match, field indices are
//!   dense in declaration order, field names are unique, every
//!   field's `ir_type` is in the alpha transient set.
//! - [`seal_struct_ops`] runs across the assembled
//!   [`IRProgram`] / [`IRScript`] (call site supplies the cross-
//!   package struct lookup) and validates every
//!   [`IRInstruction::StructInit`] / [`IRInstruction::FieldGet`]
//!   against the matching decl: target struct is registered,
//!   `StructInit::fields` matches the decl positionally and one-for-
//!   one, `FieldGet::field_index` is in range, and
//!   `FieldGet::field_type` matches `IRStructField::ir_type`.
//!
//! Both checks panic on violation through [`super::seal_panic`] —
//! struct seal failures indicate a lower / lift_signatures bug.

use std::collections::HashSet;

use crate::function::{IRFunction, IRInstruction, IRSymbol};
use crate::package::IRPackage;
use crate::struct_decl::{IRStructDecl, IRStructField};

use super::{require_supported_type, seal_panic};

pub(super) fn seal_struct_decls(pkg: &IRPackage) {
    for (sym, decl) in &pkg.structs {
        if sym != &decl.symbol {
            seal_panic(&format!(
                "package `{}` keys struct at `{sym}` but the struct's own symbol is `{}`",
                pkg.package, decl.symbol,
            ));
        }
        seal_struct_decl(decl);
    }
}

fn seal_struct_decl(decl: &IRStructDecl) {
    let owner = format!("struct `{}`", decl.symbol);
    seal_named_field_layout(&owner, &decl.fields);
}

/// Validate a named-field layout (struct decl or enum struct-variant
/// payload) against the shared invariants: dense, declaration-order
/// `index`es; unique field names; every `ir_type` in the alpha
/// supported set.
///
/// Shared helper because both [`IRStructDecl`] and
/// [`crate::IRVariantPayload::Struct`] reuse the same
/// [`IRStructField`] vocabulary — the alpha enum slice's struct
/// variants are structurally a struct's field roster, and the seal
/// surface should be identical.
pub(super) fn seal_named_field_layout(owner: &str, fields: &[IRStructField]) {
    let mut seen_names: HashSet<&str> = HashSet::with_capacity(fields.len());
    for (position, field) in fields.iter().enumerate() {
        if field.index as usize != position {
            seal_panic(&format!(
                "{owner} field `{name}` has index {index} at position {position}; \
                 indices must be dense in declaration order (0..n)",
                name = field.name,
                index = field.index,
            ));
        }
        if !seen_names.insert(field.name.as_str()) {
            seal_panic(&format!(
                "{owner} declares duplicate field name `{}`",
                field.name,
            ));
        }
        require_supported_type(&field.ir_type, &|| {
            format!("{owner} field `{}` type", field.name)
        });
    }
}

/// Cross-instruction struct check. Driven by the `(owner, inst)`
/// stream the caller produces (see [`package_instructions`] /
/// [`function_instructions`]); `lookup` resolves an
/// [`IRSymbol::mangled`] view to the registered [`IRStructDecl`]
/// (`IRProgram::struct_decl` / `IRScript::struct_decl`).
pub(super) fn seal_struct_ops<'inst, 'decl>(
    instructions: impl IntoIterator<Item = (String, &'inst IRInstruction)>,
    lookup: &impl Fn(&str) -> Option<&'decl IRStructDecl>,
) {
    for (owner, inst) in instructions {
        match inst {
            IRInstruction::StructInit {
                dest: _,
                fields,
                ty,
            } => {
                let decl = require_struct(lookup, ty, &owner);
                if fields.len() != decl.fields.len() {
                    seal_panic(&format!(
                        "{owner}: StructInit for `{ty}` carries {got} field(s) but the decl \
                         has {expected}",
                        got = fields.len(),
                        expected = decl.fields.len(),
                    ));
                }
                for (position, init) in fields.iter().enumerate() {
                    if init.index as usize != position {
                        seal_panic(&format!(
                            "{owner}: StructInit for `{ty}` field-init #{position} has index \
                             {index}; field inits must be canonicalized to declaration order",
                            index = init.index,
                        ));
                    }
                }
            }
            IRInstruction::FieldGet {
                base: _,
                dest: _,
                field_index,
                field_type,
                struct_symbol,
            } => {
                let decl = require_struct(lookup, struct_symbol, &owner);
                let Some(declared) = decl.fields.get(*field_index as usize) else {
                    seal_panic(&format!(
                        "{owner}: FieldGet on `{struct_symbol}` references field index \
                         {field_index}, but the decl only has {count} field(s)",
                        count = decl.fields.len(),
                    ));
                };
                if &declared.ir_type != field_type {
                    seal_panic(&format!(
                        "{owner}: FieldGet on `{struct_symbol}.{name}` carries field_type \
                         `{got:?}` but the decl declares `{expected:?}`",
                        name = declared.name,
                        got = field_type,
                        expected = declared.ir_type,
                    ));
                }
            }
            IRInstruction::BinaryOp { .. }
            | IRInstruction::Call { .. }
            | IRInstruction::Const { .. }
            | IRInstruction::EnumConstruct { .. }
            | IRInstruction::EnumPayloadFieldGet { .. }
            | IRInstruction::EnumTagGet { .. }
            | IRInstruction::LoadConst { .. }
            | IRInstruction::LocalDecl { .. }
            | IRInstruction::LocalRead { .. }
            | IRInstruction::LocalWrite { .. }
            | IRInstruction::UnaryOp { .. } => {}
        }
    }
}

fn require_struct<'decl>(
    lookup: &impl Fn(&str) -> Option<&'decl IRStructDecl>,
    symbol: &IRSymbol,
    owner: &str,
) -> &'decl IRStructDecl {
    lookup(symbol.mangled()).unwrap_or_else(|| {
        seal_panic(&format!(
            "{owner}: struct symbol `{symbol}` is not registered in any package",
        ))
    })
}

/// Every `(owner, instruction)` pair across every function in a
/// package. Used by both the program and script paths to feed
/// [`seal_struct_ops`].
pub(super) fn package_instructions(
    pkg: &IRPackage,
) -> impl Iterator<Item = (String, &IRInstruction)> {
    pkg.functions.values().flat_map(function_instructions)
}

/// Every `(owner, instruction)` pair across every block of a single
/// function. Owner labels carry both the function symbol and the
/// block id so seal panics point at the offending site directly.
pub(super) fn function_instructions(
    function: &IRFunction,
) -> impl Iterator<Item = (String, &IRInstruction)> {
    let owner = format!("function `{}`", function.symbol);
    function.blocks.iter().flat_map(move |block| {
        let owner = owner.clone();
        let block_id = block.id;
        block
            .instructions
            .iter()
            .map(move |inst| (format!("{owner} block {block_id}"), inst))
    })
}

/// Every `(owner, instruction)` pair across the bare blocks of an
/// `IRScript` body. Mirrors [`function_instructions`] but with a
/// `"script body block …"` owner label.
pub(super) fn script_body_instructions(
    blocks: &[crate::function::IRBasicBlock],
) -> impl Iterator<Item = (String, &IRInstruction)> {
    blocks.iter().flat_map(|block| {
        let block_id = block.id;
        block
            .instructions
            .iter()
            .map(move |inst| (format!("script body block {block_id}"), inst))
    })
}

// Inline unit coverage for the struct seal invariants. They live next to
// the helpers because [`seal_struct_decls`] / [`seal_struct_ops`] are
// `pub(super)` — integration tests under `tests/` can't reach them, and
// every other seal-violation path in the crate is exercised the same way
// (the lowering pass produces correct IR by construction, so the only way
// to drive a violation is to hand-build a malformed [`IRPackage`] /
// [`IRStructDecl`] / [`IRInstruction`] and call the helper directly).
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use expo_ast::identifier::Identifier;

    use crate::function::{IRInstruction, IRSymbol};
    use crate::package::IRPackage;
    use crate::struct_decl::{IRStructDecl, IRStructField, StructFieldInit};
    use crate::types::{IRType, ValueId};

    use super::{seal_struct_decls, seal_struct_ops};

    fn symbol(name: &str) -> IRSymbol {
        IRSymbol::from_identifier(&Identifier::new("TestApp", vec![name.to_string()]))
    }

    fn point_decl() -> IRStructDecl {
        IRStructDecl {
            symbol: symbol("Point"),
            fields: vec![
                IRStructField {
                    index: 0,
                    ir_type: IRType::Int64,
                    name: "x".to_string(),
                },
                IRStructField {
                    index: 1,
                    ir_type: IRType::Int64,
                    name: "y".to_string(),
                },
            ],
        }
    }

    fn package_with(decl: IRStructDecl) -> IRPackage {
        let mut structs = BTreeMap::new();
        structs.insert(decl.symbol.clone(), decl);
        IRPackage {
            constants: BTreeMap::new(),
            enums: BTreeMap::new(),
            functions: BTreeMap::new(),
            package: "TestApp".to_string(),
            structs,
        }
    }

    fn lookup_against<'a>(
        decls: &'a [IRStructDecl],
    ) -> impl Fn(&str) -> Option<&'a IRStructDecl> + 'a {
        move |needle: &str| decls.iter().find(|decl| decl.symbol.mangled() == needle)
    }

    #[test]
    fn well_formed_decls_pass_seal() {
        let pkg = package_with(point_decl());
        seal_struct_decls(&pkg);
    }

    #[test]
    #[should_panic(expected = "indices must be dense in declaration order")]
    fn non_monotonic_field_index_panics() {
        let mut decl = point_decl();
        decl.fields[1].index = 7;
        seal_struct_decls(&package_with(decl));
    }

    #[test]
    #[should_panic(expected = "declares duplicate field name `x`")]
    fn duplicate_field_name_panics() {
        let mut decl = point_decl();
        decl.fields[1].name = "x".to_string();
        seal_struct_decls(&package_with(decl));
    }

    #[test]
    #[should_panic(expected = "package `TestApp` keys struct at")]
    fn key_symbol_mismatch_panics() {
        let decl = point_decl();
        let mut structs = BTreeMap::new();
        structs.insert(symbol("Misnamed"), decl);
        let pkg = IRPackage {
            constants: BTreeMap::new(),
            enums: BTreeMap::new(),
            functions: BTreeMap::new(),
            package: "TestApp".to_string(),
            structs,
        };
        seal_struct_decls(&pkg);
    }

    #[test]
    fn well_formed_struct_init_passes_seal() {
        let decl = point_decl();
        let inst = IRInstruction::StructInit {
            dest: ValueId(0),
            ty: decl.symbol.clone(),
            fields: vec![
                StructFieldInit {
                    index: 0,
                    value: ValueId(1),
                },
                StructFieldInit {
                    index: 1,
                    value: ValueId(2),
                },
            ],
        };
        let decls = vec![decl];
        let lookup = lookup_against(&decls);
        seal_struct_ops(std::iter::once(("test".to_string(), &inst)), &lookup);
    }

    #[test]
    #[should_panic(expected = "is not registered in any package")]
    fn struct_init_with_unregistered_symbol_panics() {
        let inst = IRInstruction::StructInit {
            dest: ValueId(0),
            ty: symbol("Unknown"),
            fields: vec![],
        };
        let decls: Vec<IRStructDecl> = vec![];
        let lookup = lookup_against(&decls);
        seal_struct_ops(std::iter::once(("test".to_string(), &inst)), &lookup);
    }

    #[test]
    #[should_panic(expected = "carries 1 field(s) but the decl has 2")]
    fn struct_init_field_count_mismatch_panics() {
        let decl = point_decl();
        let inst = IRInstruction::StructInit {
            dest: ValueId(0),
            ty: decl.symbol.clone(),
            fields: vec![StructFieldInit {
                index: 0,
                value: ValueId(1),
            }],
        };
        let decls = vec![decl];
        let lookup = lookup_against(&decls);
        seal_struct_ops(std::iter::once(("test".to_string(), &inst)), &lookup);
    }

    #[test]
    #[should_panic(expected = "field inits must be canonicalized to declaration order")]
    fn struct_init_field_order_violation_panics() {
        let decl = point_decl();
        let inst = IRInstruction::StructInit {
            dest: ValueId(0),
            ty: decl.symbol.clone(),
            fields: vec![
                StructFieldInit {
                    index: 1,
                    value: ValueId(1),
                },
                StructFieldInit {
                    index: 0,
                    value: ValueId(2),
                },
            ],
        };
        let decls = vec![decl];
        let lookup = lookup_against(&decls);
        seal_struct_ops(std::iter::once(("test".to_string(), &inst)), &lookup);
    }

    #[test]
    #[should_panic(expected = "references field index 5, but the decl only has 2 field(s)")]
    fn field_get_out_of_range_panics() {
        let decl = point_decl();
        let inst = IRInstruction::FieldGet {
            base: ValueId(1),
            dest: ValueId(2),
            field_index: 5,
            field_type: IRType::Int64,
            struct_symbol: decl.symbol.clone(),
        };
        let decls = vec![decl];
        let lookup = lookup_against(&decls);
        seal_struct_ops(std::iter::once(("test".to_string(), &inst)), &lookup);
    }

    #[test]
    #[should_panic(expected = "carries field_type")]
    fn field_get_type_mismatch_panics() {
        let decl = point_decl();
        let inst = IRInstruction::FieldGet {
            base: ValueId(1),
            dest: ValueId(2),
            field_index: 0,
            field_type: IRType::Bool,
            struct_symbol: decl.symbol.clone(),
        };
        let decls = vec![decl];
        let lookup = lookup_against(&decls);
        seal_struct_ops(std::iter::once(("test".to_string(), &inst)), &lookup);
    }
}
