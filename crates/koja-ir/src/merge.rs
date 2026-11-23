//! The merge sub-pass stitches the per-package [`IRPackage`] fragments
//! produced by [`crate::lower::lower_package`] into a single working
//! [`IRProgram`].
//!
//! Multiple source groups may share a package label, notably when
//! testing `Global`. Coalescing them before whole-program passes keeps
//! package ownership unambiguous.

use std::collections::BTreeMap;

use crate::IRProgram;
use crate::function::IRSymbol;
use crate::package::IRPackage;
use crate::union_decl::IRUnionDecl;

/// Fold fragments that share a package label into one [`IRPackage`],
/// keeping first-seen order. A symbol declared in two fragments of
/// the same package panics. A union symbol can repeat, and panics
/// only when the two declarations differ.
pub(crate) fn coalesce(fragments: Vec<IRPackage>) -> Vec<IRPackage> {
    let mut packages: Vec<IRPackage> = Vec::new();
    for mut fragment in fragments {
        let Some(package) = packages
            .iter_mut()
            .find(|package| package.package == fragment.package)
        else {
            packages.push(fragment);
            continue;
        };
        merge_declarations(
            &mut package.constants,
            &mut fragment.constants,
            "constant",
            &package.package,
        );
        merge_declarations(
            &mut package.enums,
            &mut fragment.enums,
            "enum",
            &package.package,
        );
        merge_declarations(
            &mut package.functions,
            &mut fragment.functions,
            "function",
            &package.package,
        );
        merge_declarations(
            &mut package.structs,
            &mut fragment.structs,
            "struct",
            &package.package,
        );
        merge_unions(&mut package.unions, &mut fragment.unions, &package.package);
    }
    packages
}

/// Wrap coalesced packages into a working [`IRProgram`]. Link
/// libraries and the built constant order are collected by later
/// passes.
pub(crate) fn merge(packages: Vec<IRPackage>, entry_point: IRSymbol) -> IRProgram {
    IRProgram {
        built_constant_order: Vec::new(),
        entry_point,
        link_libraries: Vec::new(),
        packages,
    }
}

fn merge_declarations<T>(
    target: &mut BTreeMap<IRSymbol, T>,
    source: &mut BTreeMap<IRSymbol, T>,
    kind: &str,
    package: &str,
) {
    if let Some(symbol) = source.keys().find(|symbol| target.contains_key(*symbol)) {
        panic!("IR merge: duplicate {kind} `{symbol}` in package `{package}`");
    }
    target.append(source);
}

/// Move the union declarations of `source` into `target`. Each
/// fragment that names an anonymous union declares it, so a symbol
/// can repeat. A repeat must equal the declaration `target` holds.
fn merge_unions(
    target: &mut BTreeMap<IRSymbol, IRUnionDecl>,
    source: &mut BTreeMap<IRSymbol, IRUnionDecl>,
    package: &str,
) {
    for (symbol, declaration) in std::mem::take(source) {
        match target.get(&symbol) {
            Some(first) if *first != declaration => panic!(
                "IR merge: union `{symbol}` has two declarations that differ in package `{package}`"
            ),
            Some(_) => {}
            None => {
                target.insert(symbol, declaration);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::IRType;

    fn union_decl(members: Vec<IRType>) -> IRUnionDecl {
        IRUnionDecl {
            max_payload_size: 0,
            members,
            symbol: IRSymbol::synthetic("U_shared".to_string()),
        }
    }

    fn fragment_with(decl: IRUnionDecl) -> IRPackage {
        IRPackage {
            constants: BTreeMap::new(),
            enums: BTreeMap::new(),
            functions: BTreeMap::new(),
            package: "TestApp".to_string(),
            structs: BTreeMap::new(),
            unions: BTreeMap::from([(decl.symbol.clone(), decl)]),
        }
    }

    #[test]
    fn fragments_with_an_equal_union_coalesce_to_one_declaration() {
        let decl = union_decl(vec![IRType::Int64, IRType::String]);
        let packages = coalesce(vec![
            fragment_with(decl.clone()),
            fragment_with(decl.clone()),
        ]);
        assert_eq!(packages.len(), 1);
        assert_eq!(
            packages[0].unions,
            BTreeMap::from([(decl.symbol.clone(), decl)])
        );
    }

    #[test]
    #[should_panic(expected = "union `U_shared` has two declarations that differ")]
    fn fragments_with_a_union_that_differs_panic() {
        coalesce(vec![
            fragment_with(union_decl(vec![IRType::Int64, IRType::String])),
            fragment_with(union_decl(vec![IRType::Bool, IRType::String])),
        ]);
    }
}
