//! Union-shaped seal invariants.
//!
//! [`seal_union_decls`] runs per package and validates every
//! [`crate::IRUnionDecl`]. The key must match the symbol, and every
//! member type must be in the supported set. An
//! [`crate::IRType::Union`] holds a symbol only, so this is the one
//! place that checks the members. [`super::require_supported_type`]
//! proves that each union a type names has a declaration, and
//! [`super::types`] checks the instructions that build and read a
//! union value.
//!
//! The check panics on violation through [`super::seal_panic`], since
//! a union seal failure indicates a lower bug.

use crate::declarations::Declarations;
use crate::package::IRPackage;

use super::{require_supported_type, seal_panic};

pub(super) fn seal_union_decls(pkg: &IRPackage, declarations: &Declarations<'_>) {
    for (sym, decl) in &pkg.unions {
        if sym != &decl.symbol {
            seal_panic(&format!(
                "package `{}` keys union at `{sym}` but the union's own symbol is `{}`",
                pkg.package, decl.symbol,
            ));
        }
        for (idx, member) in decl.members.iter().enumerate() {
            require_supported_type(member, declarations, &|| {
                format!("union `{}` member[{idx}]", decl.symbol)
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::declarations::Declarations;
    use crate::function::IRSymbol;
    use crate::package::IRPackage;
    use crate::types::IRType;
    use crate::union_decl::IRUnionDecl;

    use super::seal_union_decls;

    fn symbol(name: &str) -> IRSymbol {
        IRSymbol::synthetic(name.to_string())
    }

    /// Seal the union decls of `pkg` against its own declarations.
    fn seal_decls(pkg: &IRPackage) {
        seal_union_decls(pkg, &Declarations::new(std::slice::from_ref(pkg)));
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

    /// A union with one member, a list of the union `inner`.
    fn list_of(inner: &str) -> IRUnionDecl {
        IRUnionDecl {
            max_payload_size: 24,
            members: vec![IRType::List(Box::new(IRType::Union(symbol(inner))))],
            symbol: symbol("U_outer"),
        }
    }

    #[test]
    fn well_formed_decls_pass_seal() {
        seal_decls(&package_with("Union_Int64_or_String", int_or_string()));
    }

    #[test]
    #[should_panic(expected = "package `TestApp` keys union at")]
    fn key_symbol_mismatch_panics() {
        seal_decls(&package_with("Misnamed", int_or_string()));
    }

    #[test]
    fn member_that_names_a_declared_union_passes_seal() {
        let mut pkg = package_with("U_outer", list_of("Union_Int64_or_String"));
        let inner = int_or_string();
        pkg.unions.insert(inner.symbol.clone(), inner);
        seal_decls(&pkg);
    }

    #[test]
    #[should_panic(
        expected = "union `U_outer` member[0] (List element) names union `U_missing`, \
                    which is not registered in any package"
    )]
    fn member_that_names_a_union_with_no_declaration_panics() {
        seal_decls(&package_with("U_outer", list_of("U_missing")));
    }
}
