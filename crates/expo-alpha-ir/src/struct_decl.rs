//! Struct-shaped top-level decls and the per-instruction payload for
//! struct construction.
//!
//! A lowered [`IRStructDecl`] keys at its [`IRSymbol`] (mangled
//! package-qualified name, mirroring [`crate::IRFunction::symbol`])
//! and carries field metadata in declaration order. Each
//! [`IRStructField`] carries its 0-based positional `index` even
//! though the slot is also implicit in the `Vec` ordering — backends
//! lift the index out of the struct and consult it directly when
//! materializing GEPs, mirroring v1 `expo-codegen`'s pattern. Field
//! names are kept for diagnostic / debug rendering and for naming
//! LLVM allocas + GEPs; lowering and seal index by position.
//!
//! `Struct{...}` literals lower to [`crate::IRInstruction::StructInit`]
//! whose `fields` are pre-canonicalized to declaration order. Each
//! [`StructFieldInit`] therefore carries `index` plus the producing
//! [`crate::ValueId`] only; the field's type comes from the matching
//! [`IRStructField::ir_type`]. Re-introducing per-init types is a
//! follow-up if generics start producing field-substituted
//! instantiations that diverge from the declaration shape.

use crate::function::IRSymbol;
use crate::types::{IRType, ValueId};

/// One field of an [`IRStructDecl`]. `index` is the field's 0-based
/// position in declaration order; backends use it as the LLVM struct
/// field index for GEPs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IRStructField {
    pub index: u32,
    pub ir_type: IRType,
    pub name: String,
}

/// A lowered struct declaration. `symbol` is the same package-qualified
/// mangled name shape an [`crate::IRFunction`] uses; `fields` is the
/// declaration-order field list. Empty `fields` is legal (matches
/// `struct Foo / end` shape).
#[derive(Debug, Clone)]
pub struct IRStructDecl {
    pub fields: Vec<IRStructField>,
    pub symbol: IRSymbol,
}

/// One field initializer inside an [`crate::IRInstruction::StructInit`].
/// `index` selects the declared field by position; `value` is the
/// already-lowered [`ValueId`] producing the field's value. Lowering
/// produces these in declaration order so seal / backends iterate
/// linearly without re-sorting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructFieldInit {
    pub index: u32,
    pub value: ValueId,
}
