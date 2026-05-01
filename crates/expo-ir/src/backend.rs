//! Execution backends for [`crate::IRProgram`].
//!
//! A backend consumes a fully-elaborated [`crate::IRProgram`] and exposes
//! a way to invoke functions and observe their results. Today's
//! implementors live downstream:
//!
//! - **`expo-ir-eval`** -- a tree-walking interpreter that produces
//!   `Value`s.
//! - **(planned) `expo-ir-cranelift`** -- Cranelift JIT for the REPL's
//!   warm path.
//!
//! AOT-style backends (LLVM `.o`, C, WASM) follow a different shape
//! (emit an artifact rather than respond to `call`); they will land
//! under a sibling trait (`CodeEmitter`) when the codegen rewrite
//! happens. This trait covers the execution shape only.
//!
//! Backends rely on the IR contract enforced by
//! [`crate::IRProgram::validate`]: no [`crate::IRInstruction::Stub`],
//! no [`crate::IRInstruction::FromListLiteral`], no
//! [`crate::IRInstruction::UnionWrap`] -- all of those are upstream
//! pass responsibilities. Calling [`Backend::new`] on an invalid
//! program returns the underlying validation error rather than failing
//! deep in dispatch.

use std::sync::Arc;

use expo_typecheck::context::TypeContext;

use crate::identity::FunctionIdentifier;
use crate::program::IRProgram;

pub trait Backend {
    /// The runtime value produced by `call` and accepted as an argument.
    type Value;

    /// Backend-specific error returned by `new` and `call`.
    type Error;

    /// Construct a backend over a sealed `IRProgram`. Implementations
    /// should call [`IRProgram::validate`] (or call-site-equivalent
    /// invariants) early so callers get a structured error rather
    /// than a panic deep in dispatch.
    fn new(program: Arc<IRProgram>, type_ctx: Arc<TypeContext>) -> Result<Self, Self::Error>
    where
        Self: Sized;

    /// Invoke a function by mangled identifier and return its result.
    fn call(
        &mut self,
        callee: &FunctionIdentifier,
        args: Vec<Self::Value>,
    ) -> Result<Self::Value, Self::Error>;

    /// Pretty-print a value. Each backend owns its `Value` type, so
    /// formatting lives behind the trait.
    fn format_value(&self, value: &Self::Value) -> String;
}
