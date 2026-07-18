//! Merge sub-pass: stitch the per-package [`IRPackage`] fragments
//! produced by [`crate::lower_package`] into a single working
//! [`IRProgram`].
//!
//! Multiple source groups may share a package label, notably when
//! testing `Global`. Coalescing them before whole-program passes keeps
//! package ownership unambiguous.

use std::collections::BTreeMap;

use crate::IRProgram;
use crate::function::IRSymbol;
use crate::package::IRPackage;

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
        merge_declarations(
            &mut package.unions,
            &mut fragment.unions,
            "union",
            &package.package,
        );
    }
    packages
}

pub(crate) fn merge(packages: Vec<IRPackage>, entry_point: IRSymbol) -> IRProgram {
    IRProgram {
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
