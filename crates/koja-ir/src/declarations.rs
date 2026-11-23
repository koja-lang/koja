//! One read-only index over every declaration in a package set.
//!
//! Packages hold their declarations in per-kind maps, so a
//! cross-package lookup visits each package in turn. The passes that
//! look up many symbols build a [`Declarations`] once and read from
//! it. Building the index also proves that every constant, enum,
//! function, and struct symbol is declared in exactly one package,
//! and that the copies of a union declaration are equal. Backends
//! that make one-off lookups by mangled string go through
//! [`find_in`] instead.

use std::collections::BTreeMap;

use crate::constant::IRConstantValue;
use crate::enum_decl::IREnumDecl;
use crate::function::{IRFunction, IRSymbol};
use crate::package::IRPackage;
use crate::struct_decl::IRStructDecl;
use crate::types::IRType;
use crate::union_decl::{IRUnionDecl, union_members};

/// Read-only declaration indexes over a package set, keyed by
/// symbol.
pub(crate) struct Declarations<'a> {
    constants: BTreeMap<&'a IRSymbol, &'a IRConstantValue>,
    enums: BTreeMap<&'a IRSymbol, &'a IREnumDecl>,
    functions: BTreeMap<&'a IRSymbol, &'a IRFunction>,
    structs: BTreeMap<&'a IRSymbol, &'a IRStructDecl>,
    unions: BTreeMap<&'a IRSymbol, &'a IRUnionDecl>,
}

impl<'a> Declarations<'a> {
    /// Index every declaration in `packages`. Panics when a constant,
    /// enum, function, or struct symbol is declared in more than one
    /// package, or when two declarations of one union symbol differ.
    pub(crate) fn new(packages: &'a [IRPackage]) -> Self {
        let mut declarations = Self {
            constants: BTreeMap::new(),
            enums: BTreeMap::new(),
            functions: BTreeMap::new(),
            structs: BTreeMap::new(),
            unions: BTreeMap::new(),
        };
        for package in packages {
            index_unique(&mut declarations.constants, &package.constants, "constant");
            index_unique(&mut declarations.enums, &package.enums, "enum");
            index_unique(&mut declarations.functions, &package.functions, "function");
            index_unique(&mut declarations.structs, &package.structs, "struct");
            index_unions(&mut declarations.unions, &package.unions);
        }
        declarations
    }

    pub(crate) fn constant_value(&self, symbol: &IRSymbol) -> Option<&'a IRConstantValue> {
        self.constants.get(symbol).copied()
    }

    /// Every constant, in symbol order.
    pub(crate) fn constants(&self) -> impl Iterator<Item = (&'a IRSymbol, &'a IRConstantValue)> {
        self.constants
            .iter()
            .map(|(symbol, value)| (*symbol, *value))
    }

    pub(crate) fn enum_decl(&self, symbol: &IRSymbol) -> Option<&'a IREnumDecl> {
        self.enums.get(symbol).copied()
    }

    pub(crate) fn function(&self, symbol: &IRSymbol) -> Option<&'a IRFunction> {
        self.functions.get(symbol).copied()
    }

    /// Every function, in symbol order.
    pub(crate) fn functions(&self) -> impl Iterator<Item = &'a IRFunction> {
        self.functions.values().copied()
    }

    pub(crate) fn struct_decl(&self, symbol: &IRSymbol) -> Option<&'a IRStructDecl> {
        self.structs.get(symbol).copied()
    }

    pub(crate) fn union_decl(&self, symbol: &IRSymbol) -> Option<&'a IRUnionDecl> {
        self.unions.get(symbol).copied()
    }
}

/// The members of the union type `ty` in canonical order, or `None`
/// when `ty` is not a union. The members live on the declaration,
/// which this finds in `packages`. For a pass or a backend that
/// holds the packages and no [`Declarations`].
pub fn union_members_in<'a>(packages: &'a [IRPackage], ty: &IRType) -> Option<&'a [IRType]> {
    union_members(ty, |symbol| {
        find_in(packages, |package| &package.unions, symbol.mangled())
    })
}

/// Find `mangled` in the per-package map that `select` picks,
/// visiting packages in order. `O(packages * log entries)`, which is
/// cheap for the one to three packages a program ships today.
pub(crate) fn find_in<'a, T>(
    packages: &'a [IRPackage],
    select: impl Fn(&'a IRPackage) -> &'a BTreeMap<IRSymbol, T>,
    mangled: &str,
) -> Option<&'a T> {
    packages
        .iter()
        .find_map(|package| select(package).get(mangled))
}

fn index_unique<'a, T>(
    index: &mut BTreeMap<&'a IRSymbol, &'a T>,
    declarations: &'a BTreeMap<IRSymbol, T>,
    kind: &str,
) {
    for (symbol, declaration) in declarations {
        if index.insert(symbol, declaration).is_some() {
            panic!(
                "IR seal violation: {kind} symbol `{symbol}` is registered in more than one package"
            );
        }
    }
}

/// Index the union declarations of one package. Each package that
/// names an anonymous union declares it, so a symbol can repeat. The
/// index keeps the first declaration, and a repeat must equal it.
fn index_unions<'a>(
    index: &mut BTreeMap<&'a IRSymbol, &'a IRUnionDecl>,
    declarations: &'a BTreeMap<IRSymbol, IRUnionDecl>,
) {
    for (symbol, declaration) in declarations {
        let first = *index.entry(symbol).or_insert(declaration);
        if first != declaration {
            panic!("IR seal violation: union symbol `{symbol}` has two declarations that differ");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn union_decl(members: Vec<IRType>) -> IRUnionDecl {
        IRUnionDecl {
            max_payload_size: 0,
            members,
            symbol: IRSymbol::synthetic("U_shared".to_string()),
        }
    }

    fn package_with(name: &str, decl: IRUnionDecl) -> IRPackage {
        IRPackage {
            constants: BTreeMap::new(),
            enums: BTreeMap::new(),
            functions: BTreeMap::new(),
            package: name.to_string(),
            structs: BTreeMap::new(),
            unions: BTreeMap::from([(decl.symbol.clone(), decl)]),
        }
    }

    #[test]
    fn an_equal_repeat_of_a_union_indexes_one_declaration() {
        let decl = union_decl(vec![IRType::Int64, IRType::String]);
        let packages = [
            package_with("First", decl.clone()),
            package_with("Second", decl.clone()),
        ];
        let declarations = Declarations::new(&packages);
        assert_eq!(declarations.union_decl(&decl.symbol), Some(&decl));
    }

    #[test]
    #[should_panic(expected = "union symbol `U_shared` has two declarations that differ")]
    fn a_repeat_of_a_union_that_differs_panics() {
        let packages = [
            package_with("First", union_decl(vec![IRType::Int64, IRType::String])),
            package_with("Second", union_decl(vec![IRType::Bool, IRType::String])),
        ];
        Declarations::new(&packages);
    }
}
