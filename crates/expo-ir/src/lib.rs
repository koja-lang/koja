//! ExpoIR: home for the LLVM-free decision types and lowering helpers that
//! sit between the typed AST and codegen backends.
//!
//! Today the crate hosts the `Resolved*` decision-type vocabulary
//! ([`resolved`]) and the freestanding lowering helpers ([`lower`]) that
//! produce them, plus shared semantic state ([`FnLowerState`],
//! [`TypeLayouts`]) and transitional identities (see [`identity`]).
//! The full SIL-style instruction containers (function, basic block,
//! instruction sequence) are intentionally undefined in code -- their shape
//! will be discovered bottom-up during the lowering/emission split, driven
//! by what `Resolved*` consumers need to be stitched together. See
//! `expo/design/EXPOIR-ROADMAP.md` for design intent and current phase status.

pub mod blocks;
pub mod cfg;
pub mod elaborate;
mod fn_state;
pub mod identity;
pub mod lower;
pub mod ownership;
pub mod program;
pub mod resolved;
mod type_layouts;
pub mod util;
pub mod values;

pub use blocks::{IRBasicBlock, IRBlockId, IRTerminator};
pub use cfg::CFGBuilder;
pub use elaborate::elaborate_program;
pub use fn_state::FnLowerState;
pub use identity::{FunctionIdentifier, MonomorphizedTypeIdentifier, VariantIdentifier};
pub use lower::Lowerer;
pub use ownership::Ownership;
pub use program::{
    ExternAbi, ExternAttrs, IREnum, IRFunction, IRFunctionKind, IRFunctionMeta, IRParam, IRProgram,
    IRStruct, IRStructKind,
};
pub use type_layouts::TypeLayouts;
pub use values::{IRInstruction, IROperand, IRValueId};
