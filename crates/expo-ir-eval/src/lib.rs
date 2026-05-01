//! Tree-walking interpreter backend for [`expo_ir::IRProgram`].
//!
//! This is one of two planned execution backends for ExpoIR (the other
//! being a Cranelift JIT for the REPL's warm path). It implements
//! [`expo_ir::Backend`] by walking IR blocks and dispatching each
//! [`expo_ir::IRInstruction`] to a Rust handler.
//!
//! Today's primary consumer is `expo-shell` (the REPL); a secondary
//! goal is fast feedback for typecheck and IR development -- the
//! interpreter avoids LLVM compile + link, so test programs run in
//! milliseconds instead of seconds.
//!
//! Module layout:
//!
//! - [`interp`] -- the [`Interp`] struct, [`expo_ir::Backend`] impl,
//!   and the per-instruction dispatch.
//! - [`aggregates`] -- struct/enum/variant payload construction and
//!   field-walk projection.
//! - [`binary`] -- `<<...>>` binary-literal packing.
//! - [`concat`] -- `<>` string / binary concatenation.
//! - [`constants`] -- runtime materialization of
//!   [`expo_ir::IRProgram::constants`] entries into [`Value`]s,
//!   indexed by [`expo_ir::IRConstId`] at [`Interp::new`].
//! - [`control`] -- block lookup, terminator interpretation, parameter
//!   binding.
//! - [`format`] -- `#{...}` string-interpolation assembly.
//! - [`ops`] -- pure binary/unary operator evaluation.
//! - [`pattern`] -- per-arm `Pattern*` instruction helpers (literal
//!   compare, struct-field projection, named binding).
//! - [`frame`] -- the per-call [`Frame`] and operand `materialize`.
//! - [`value`] -- the [`Value`] enum and composite payload types.
//! - [`error`] -- the [`RuntimeError`] enum.
//!
//! Coverage and limitations:
//!
//! - **Pure compute only** in MVP: no `spawn`/`receive`, no FFI.
//!   These return [`error::RuntimeError::Unsupported`] until later
//!   phases wire up the runtime hook.
//! - **Sealed IR required**: programs containing
//!   [`expo_ir::IRInstruction::Stub`],
//!   [`expo_ir::IRInstruction::FromListLiteral`], or
//!   [`expo_ir::IRInstruction::UnionWrap`] are rejected at
//!   construction time via [`expo_ir::IRProgram::validate`].

mod aggregates;
mod binary;
mod concat;
mod constants;
mod control;
pub mod error;
mod format;
pub mod frame;
pub mod interp;
mod ops;
mod pattern;
pub mod value;

pub use error::RuntimeError;
pub use frame::Frame;
pub use interp::Interp;
pub use value::{ClosureValue, EnumValue, StructValue, Value, VariantPayload};
