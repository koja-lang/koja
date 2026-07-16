//! Pooled compound constants. Strings, binaries, unit enum variants,
//! and structs of literals get one entry per top-level
//! `const NAME = <compound-rhs>` declaration. Primitives don't pool
//! (they inline as [`crate::IRInstruction::Const`] at every use).
//!
//! Each [`IRPackage`] owns its constants pool keyed by the
//! constant's mangled [`IRSymbol`], and [`crate::IRInstruction::LoadConst`]
//! carries the same symbol so backends can lazily materialize one
//! global per pool entry.
//!
//! [`IRPackage`]: crate::IRPackage

use crate::enum_decl::IRVariantTag;
use crate::function::IRSymbol;
use crate::types::ConstValue;

/// One pooled constant value. The struct/enum recursive shapes hold
/// further [`IRConstantValue`]s for their fields/payload, so backends
/// can materialize a deeply-nested constant in a single walk without
/// re-running typecheck. `Primitive` reuses the same scalar
/// [`ConstValue`] vocabulary the inline `IRInstruction::Const` path
/// uses, which keeps the two paths convergent for backends and avoids
/// re-encoding primitive literal shapes.
#[derive(Clone, Debug, PartialEq)]
pub enum IRConstantValue {
    /// `<ty>.<variant>`: a unit-shaped enum variant. `tag` is the
    /// variant's 0-based position in the [`crate::IREnumDecl`]
    /// variant roster (mirrors [`crate::IRInstruction::EnumConstruct`]'s
    /// `tag`).
    EnumVariant { tag: IRVariantTag, ty: IRSymbol },
    /// A scalar constant nested inside a compound, or a standalone
    /// heap-payload pool entry (string, binary, bits). Top-level
    /// scalar primitive constants do not pool (they inline at use
    /// sites).
    Primitive(ConstValue),
    /// `<ty>{<fields>}`: a struct literal whose fields are themselves
    /// constant values. Field order matches declaration order
    /// (mirrors [`crate::IRInstruction::StructInit`]).
    Struct {
        fields: Vec<IRConstantValue>,
        ty: IRSymbol,
    },
}
