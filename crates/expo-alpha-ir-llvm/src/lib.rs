//! LLVM backend for sealed [`expo_alpha_ir::IRProgram`]s and
//! [`expo_alpha_ir::IRScript`]s — peer to
//! [`expo-alpha-ir-eval`](../expo_alpha_ir_eval/index.html), but
//! emitting native object code via [`inkwell`] instead of
//! interpreting in-process.
//!
//! # Slice scope
//!
//! Lowers a sealed IR whose entry / script-body returns
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
//! Two pairs of entry points, one per IR shape:
//!
//! - [`compile_program`] / [`emit_llvm_ir`] — project-mode source
//!   whose user-declared `fn main` lowered through
//!   `expo-alpha-ir::lower_program`.
//! - [`compile_script`] / [`emit_script_llvm_ir`] — script-mode
//!   source whose top-level statements lowered through
//!   `expo-alpha-ir::lower_script`.
//!
//! All four mirror `expo-alpha-ir-eval`'s `Interpreter::run_*` pair:
//! one shared block-emission seam ([`compiler::Compiler::emit_as_main`])
//! drives both compile paths, with the only difference being whether
//! "main's body" comes from `program.entry_function()` or directly
//! from `script.blocks` / `script.return_type`.
//!
//! `compile_*` writes a native object file at the requested path;
//! linking lives in `expo-driver` so this crate stays free of
//! `boring-sys` and runtime archives.

mod compiler;
mod emit;
mod error;
mod lower;
mod types;

pub use error::LlvmError;

use std::path::Path;

use expo_alpha_ir::{IRProgram, IRScript};

/// Compile a sealed [`IRProgram`] to a native object file at
/// `output`. The caller is responsible for linking the resulting
/// object into an executable; `expo-driver`'s `pipeline::link` covers
/// the v1 + alpha case with the same `cc` + runtime + BoringSSL
/// invocation.
pub fn compile_program(program: &IRProgram, output: &Path) -> Result<(), LlvmError> {
    let context = inkwell::context::Context::create();
    let compiler = compiler::Compiler::new(&context);
    compiler.compile_program(program)?;
    emit::emit_object_file(compiler.module(), output)
}

/// Compile a sealed [`IRProgram`] and return its LLVM IR text. Used
/// for fast snapshot-style coverage of the lowering rules — no
/// linking, no subprocess.
pub fn emit_llvm_ir(program: &IRProgram) -> Result<String, LlvmError> {
    let context = inkwell::context::Context::create();
    let compiler = compiler::Compiler::new(&context);
    compiler.compile_program(program)?;
    Ok(compiler.module().print_to_string().to_string())
}

/// Compile a sealed [`IRScript`] to a native object file at
/// `output`. Counterpart to [`compile_program`] for script-mode
/// sources (`expo run <bare-file>`, `expo eval`, REPL fragments).
pub fn compile_script(script: &IRScript, output: &Path) -> Result<(), LlvmError> {
    let context = inkwell::context::Context::create();
    let compiler = compiler::Compiler::new(&context);
    compiler.compile_script(script)?;
    emit::emit_object_file(compiler.module(), output)
}

/// Compile a sealed [`IRScript`] and return its LLVM IR text.
/// Counterpart to [`emit_llvm_ir`] for script-mode sources; used by
/// the snapshot tests in `tests/emit.rs`.
pub fn emit_script_llvm_ir(script: &IRScript) -> Result<String, LlvmError> {
    let context = inkwell::context::Context::create();
    let compiler = compiler::Compiler::new(&context);
    compiler.compile_script(script)?;
    Ok(compiler.module().print_to_string().to_string())
}
