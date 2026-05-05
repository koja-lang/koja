//! LLVM backend for sealed [`expo_alpha_ir::IRProgram`]s and
//! [`expo_alpha_ir::IRScript`]s — peer to
//! [`expo-alpha-ir-eval`](../expo_alpha_ir_eval/index.html) but
//! emitting native object code via [`inkwell`] instead of
//! interpreting in-process.
//!
//! # Slice scope
//!
//! Emits a single-module LLVM IR program with one external `main`
//! symbol of signature `i64 ()`. `main` always returns 0 — the
//! body's value is fed to a runtime printer
//! ([`expo-runtime/src/alpha.rs`](../../expo-runtime/src/alpha.rs))
//! before the return so the binary's observable behavior matches
//! the eval interpreter's `print value, exit 0` contract. Temporary
//! scaffolding; goes away with `IO.puts`.
//!
//! Supported IR vocabulary:
//!
//! - `Const(Bool, Int8..Int64, UInt8..UInt64)`.
//! - `BinaryOp::{Add, And, Eq, Gt, GtEq, Lt, LtEq, NotEq, Or}` —
//!   `Sub`/`Mul`/`Div`/`Mod` are feature-gap follow-ups.
//! - `UnaryOp::{Neg, Not}`.
//! - `Call` — direct calls to functions declared in the same
//!   module, resolved by mangled name. Param `ValueId`s are seeded
//!   into the body's value map up front so any future
//!   parameter-reference lowering already finds its operands.
//! - `Return`.
//!
//! # Public API
//!
//! Two pairs of entry points, one per IR shape:
//!
//! - [`compile_program`] / [`emit_llvm_ir`] for project-mode source
//!   lowered through `expo-alpha-ir::lower_program`.
//! - [`compile_script`] / [`emit_script_llvm_ir`] for script-mode
//!   source lowered through `expo-alpha-ir::lower_script`.
//!
//! `compile_*` writes a native object file at the requested path;
//! linking lives in `expo-driver`.

mod compiler;
mod emit;
mod error;
mod object;
mod types;

pub use error::LlvmError;

use std::path::Path;

use expo_alpha_ir::{IRProgram, IRScript};
use inkwell::context::Context;

/// Compile a sealed [`IRProgram`] to a native object file at
/// `output`. `app_name` is embedded as the runtime's
/// `__expo_app_name` global (panic-backtrace label); convention is
/// the binary's stem. Caller links the object into an executable.
pub fn compile_program(
    program: &IRProgram,
    app_name: &str,
    output: &Path,
) -> Result<(), LlvmError> {
    let context = Context::create();
    let compiler = compiler::Compiler::new(&context);
    compiler.compile_program(program, app_name)?;
    object::emit_object_file(compiler.module(), output)
}

/// Compile a sealed [`IRProgram`] and return its LLVM IR text — for
/// snapshot-style coverage in `tests/emit.rs`. No linking, no
/// subprocess.
pub fn emit_llvm_ir(program: &IRProgram, app_name: &str) -> Result<String, LlvmError> {
    let context = Context::create();
    let compiler = compiler::Compiler::new(&context);
    compiler.compile_program(program, app_name)?;
    Ok(compiler.module().print_to_string().to_string())
}

/// Counterpart to [`compile_program`] for script-mode sources.
pub fn compile_script(script: &IRScript, app_name: &str, output: &Path) -> Result<(), LlvmError> {
    let context = Context::create();
    let compiler = compiler::Compiler::new(&context);
    compiler.compile_script(script, app_name)?;
    object::emit_object_file(compiler.module(), output)
}

/// Counterpart to [`emit_llvm_ir`] for script-mode sources.
pub fn emit_script_llvm_ir(script: &IRScript, app_name: &str) -> Result<String, LlvmError> {
    let context = Context::create();
    let compiler = compiler::Compiler::new(&context);
    compiler.compile_script(script, app_name)?;
    Ok(compiler.module().print_to_string().to_string())
}
