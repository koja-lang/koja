//! Sealed `IRProgram` lowering for the [`COMPILER-NORTHSTAR.md`]
//! pipeline.
//!
//! [`COMPILER-NORTHSTAR.md`]: ../../design/COMPILER-NORTHSTAR.md
//!
//! The single public entry point is [`lower_program`]. It consumes a
//! sealed [`expo_alpha_typecheck::CheckedProgram`], runs every sub-pass
//! internally (`lower_package`, `merge`, `seal`), and hands back a
//! sealed [`IRProgram`] on success or a [`LowerError`] for the one
//! user-actionable failure mode (entry point not registered).
//!
//! Diagnostics: lowering is a pure translation from a sealed input.
//! User-actionable errors funnel through [`LowerError`]; everything
//! else is a compiler bug and panics through `seal_program`.
//!
//! Hard contract: this crate has **zero dependency on `expo-ir`**. The
//! IR vocabulary defined here ([`IRProgram`], [`IRPackage`],
//! [`IRFunction`], [`IRInstruction`], [`IRTerminator`], …) is fresh
//! and self-contained.

mod function;
mod lower_package;
mod merge;
mod package;
mod program;
mod seal;
mod types;

pub use function::{IRBasicBlock, IRFunction, IRInstruction, IRTerminator};
pub use package::IRPackage;
pub use program::{IRProgram, LowerError, lower_program};
pub use types::{ConstValue, IRBinOp, IRType, ValueId};
