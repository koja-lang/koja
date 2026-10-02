//! One read-only index over every declaration in a package set.
//!
//! Packages hold their declarations in per-kind maps, so a
//! cross-package lookup visits each package in turn. The passes that
//! look up many symbols build a [`Declarations`] once and read from
//! it. Building the index also proves that every mangled symbol is
//! declared in exactly one package. Backends that make one-off
//! lookups by mangled string go through [`find_in`] instead.

use std::collections::BTreeMap;

use crate::constant::IRConstantValue;
use crate::enum_decl::IREnumDecl;
use crate::function::{IRFunction, IRSymbol};
use crate::package::IRPackage;
use crate::struct_decl::IRStructDecl;
use crate::union_decl::IRUnionDecl;

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
    /// Index every declaration in `packages`. Panics when one symbol
    /// is declared in more than one package.
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
            index_unique(&mut declarations.unions, &package.unions, "union");
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
