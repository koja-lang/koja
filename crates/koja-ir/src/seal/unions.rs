//! Union-shaped seal invariants.
//!
//! [`seal_union_decls`] runs per package and validates every
//! [`crate::IRUnionDecl`] in isolation. The key must match the
//! symbol, and every member type must be in the supported set. An
//! [`crate::IRType::Union`] holds a symbol only, so this is the one
//! place that checks the members. [`super::types`] checks the
//! instructions that build and read a union value.
//!
//! The check panics on violation through [`super::seal_panic`], since
//! a union seal failure indicates a lower bug.

use crate::package::IRPackage;

use super::{require_supported_type, seal_panic};

pub(super) fn seal_union_decls(pkg: &IRPackage) {
    for (sym, decl) in &pkg.unions {
        if sym != &decl.symbol {
            seal_panic(&format!(
                "package `{}` keys union at `{sym}` but the union's own symbol is `{}`",
                pkg.package, decl.symbol,
            ));
        }
        for (idx, member) in decl.members.iter().enumerate() {
            require_supported_type(member, &|| format!("union `{}` member[{idx}]", decl.symbol));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::function::IRSymbol;
    use crate::package::IRPackage;
    use crate::types::IRType;
    use crate::union_decl::IRUnionDecl;

    use super::seal_union_decls;

    fn symbol(name: &str) -> IRSymbol {
        IRSymbol::synthetic(name.to_string())
    }

    fn package_with(key: &str, decl: IRUnionDecl) -> IRPackage {
        IRPackage {
            constants: BTreeMap::new(),
            enums: BTreeMap::new(),
            functions: BTreeMap::new(),
            package: "TestApp".to_string(),
            structs: BTreeMap::new(),
            unions: BTreeMap::from([(symbol(key), decl)]),
        }
    }

    fn int_or_string() -> IRUnionDecl {
        IRUnionDecl {
            max_payload_size: 8,
            members: vec![IRType::Int64, IRType::String],
            symbol: symbol("Union_Int64_or_String"),
        }
    }

    #[test]
    fn well_formed_decls_pass_seal() {
        seal_union_decls(&package_with("Union_Int64_or_String", int_or_string()));
    }

    #[test]
    #[should_panic(expected = "package `TestApp` keys union at")]
    fn key_symbol_mismatch_panics() {
        seal_union_decls(&package_with("Misnamed", int_or_string()));
    }
}
