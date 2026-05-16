//! Per-package IR fragment. Output of the `lower-package` sub-pass and
//! input to `merge`. A `IRPackage` is the cacheable unit per the
//! northstar incremental story (one package, one cache entry).

use std::collections::BTreeMap;

use crate::constant::IRConstantValue;
use crate::enum_decl::IREnumDecl;
use crate::function::{IRFunction, IRSymbol};
use crate::struct_decl::IRStructDecl;
use crate::union_decl::IRUnionDecl;

/// Per-package IR fragment exposed to backends. Holds only
/// fully-monomorphized concrete decls — generic templates and the
/// instantiation set are pipeline-internal scratch state owned by
/// [`crate::generics`] and dropped before the sealed [`IRProgram`]
/// is returned, so backends never observe a generic template.
#[derive(Debug, Clone)]
pub struct IRPackage {
    /// Compound-constant pool keyed by mangled [`IRSymbol`]. One
    /// entry per package-level `const NAME = <compound-rhs>` whose
    /// RHS doesn't fold into a scalar [`crate::ConstValue`]
    /// (strings, unit enum variants, structs of literals). Primitive
    /// constants are inlined as [`crate::IRInstruction::Const`] at
    /// every use site and never appear here. [`crate::IRInstruction::LoadConst`]
    /// references entries through the same symbol so backends emit
    /// at most one global per pool entry.
    pub constants: BTreeMap<IRSymbol, IRConstantValue>,
    /// Enum declarations owned by this package, keyed by their
    /// stable [`IRSymbol`]. Same key shape as [`Self::structs`];
    /// backends use the symbol to consult the declared variant
    /// roster for `IRType::Enum(symbol)` /
    /// `IRInstruction::EnumConstruct`.
    pub enums: BTreeMap<IRSymbol, IREnumDecl>,
    /// Functions owned by this package, keyed by their stable
    /// [`IRSymbol`]. Each function's `symbol` field equals its key
    /// here by construction. Backends look up by `&str` through the
    /// `IRSymbol: Borrow<str>` impl, e.g.
    /// `pkg.functions.get(callee.mangled())`.
    pub functions: BTreeMap<IRSymbol, IRFunction>,
    /// The package label (e.g. `"TestApp"`, `"Global"`). Matches
    /// `CheckedPackage::package` from `expo-typecheck`.
    pub package: String,
    /// Struct declarations owned by this package, keyed by their
    /// stable [`IRSymbol`]. Same key shape as
    /// [`Self::functions`]; backends use the symbol to consult the
    /// declared field layout for `IRType::Struct(symbol)` /
    /// `IRInstruction::StructInit` / `IRInstruction::FieldGet`.
    pub structs: BTreeMap<IRSymbol, IRStructDecl>,
    /// Union declarations discovered during this package's lower —
    /// one entry per distinct mangled `IRType::Union { mangled }`
    /// referenced anywhere in the package. Backends consult
    /// `max_payload_size` to size the trailing `[N x i8]` payload
    /// in the `{ i8, [N x i8] }` LLVM struct layout.
    pub unions: BTreeMap<IRSymbol, IRUnionDecl>,
}
