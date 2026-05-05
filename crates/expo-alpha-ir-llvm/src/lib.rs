//! LLVM backend for sealed [`expo_alpha_ir::IRProgram`]s — peer to
//! [`expo-alpha-ir-eval`](../expo_alpha_ir_eval/index.html), but
//! emitting native object code via [`inkwell`] instead of
//! interpreting in-process.
//!
//! # Slice scope
//!
//! Lowers a sealed `IRProgram` whose entry function returns
//! `IRType::Int64` to a single-module LLVM IR program with one
//! external symbol (`main`) of signature `i64 ()`. The host operating
//! system truncates the i64 return value to its 8-bit exit code,
//! which is how `cargo test` and the expo driver assert behavior.
//!
//! Supported IR vocabulary: `Const(Int64 / Bool / Unit)`,
//! `BinaryOp::Add`, `Return`. Any other instruction or terminator
//! triggers a feature-gap panic — those follow on with the matching
//! IR slices.
//!
//! # Why we mirror v1's link contract but skip its entry-point wrapper
//!
//! v1 codegen wraps user `fn main` in `__expo_user_main` and emits an
//! LLVM `main` that calls `expo_rt_spawn(...)` then `expo_rt_main_done`
//! and finally `ret 0`. The wrapper is mandatory once Expo grows
//! Process / spawn semantics, but adopting it now would silently
//! discard the user's return value and force tests to observe through
//! some other channel (stdout, runtime exit-code setter). For the
//! slice we emit a direct `i64 main()` so the smoke test can assert
//! the exit code matches the expression value. The runtime libraries
//! are still statically linked at the driver layer so any future
//! runtime call lights up without rewiring the build.
//!
//! # Public API
//!
//! [`compile_program`] is the single entry point, mirroring
//! `expo-alpha-ir-eval`'s `Interpreter::run`. It writes a native
//! object file at the requested path and returns; linking lives in
//! `expo-driver` so this crate stays free of `boring-sys` and
//! runtime archives.

mod compiler;
mod emit;
mod error;
mod lower;
mod types;

pub use error::LlvmError;

use std::path::Path;

use expo_alpha_ir::IRProgram;

/// Compile a sealed [`IRProgram`] to a native object file at
/// `output`. The caller is responsible for linking the resulting
/// object into an executable; `expo-driver`'s `pipeline::link` covers
/// the v1 + alpha case with the same `cc` + runtime + BoringSSL
/// invocation.
pub fn compile_program(program: &IRProgram, output: &Path) -> Result<(), LlvmError> {
    let context = inkwell::context::Context::create();
    let compiler = compiler::Compiler::new(&context);
    compiler.compile(program)?;
    emit::emit_object_file(compiler.module(), output)
}

/// Compile a sealed [`IRProgram`] and return its LLVM IR text. Used
/// for fast snapshot-style coverage of the lowering rules — no
/// linking, no subprocess.
pub fn emit_llvm_ir(program: &IRProgram) -> Result<String, LlvmError> {
    let context = inkwell::context::Context::create();
    let compiler = compiler::Compiler::new(&context);
    compiler.compile(program)?;
    Ok(compiler.module().print_to_string().to_string())
}
