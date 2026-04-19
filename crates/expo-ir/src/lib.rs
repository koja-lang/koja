//! ExpoIR: the intermediate representation between the typed AST and codegen
//! backends.
//!
//! Inspired by Swift's SIL rather than Rust's MIR, ExpoIR preserves high-level
//! semantics (ownership operations, enum switching, struct construction) so
//! multiple backends can lower independently. The lowering pass reads
//! `resolved_type` from the typed AST and produces flat, explicit IR;
//! emission is mechanical: walk IR instructions, emit target-specific code.

mod file;
mod instruction;
pub mod lower;
pub mod resolved;
mod type_layouts;
mod types;

pub use file::{IRBasicBlock, IRFile, IRFunction, IRStruct};
pub use instruction::{IRInstruction, IRTerminator};
pub use type_layouts::TypeLayouts;
pub use types::{IRBuiltinOp, IROperand, IRType, IRVar};
