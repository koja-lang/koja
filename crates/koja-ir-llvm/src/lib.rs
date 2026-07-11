//! LLVM backend for sealed [`koja_ir::IRProgram`]s and
//! [`koja_ir::IRScript`]s: peer to
//! [`koja-ir-eval`](../koja_ir_eval/index.html) but
//! emitting native object code via [`inkwell`] instead of
//! interpreting in-process.
//!
//! # Slice scope
//!
//! Emits a single-module LLVM IR program with one external `main`
//! symbol of signature `i64 ()`. `main` always returns 0. The
//! body's value is fed to a runtime printer
//! ([`koja-runtime-posix/src/intrinsics.rs`](../../koja-runtime-posix/src/intrinsics.rs))
//! before the return so the binary's observable behavior matches
//! the eval interpreter's `print value, exit 0` contract. Temporary
//! scaffolding that goes away with `IO.puts`.
//!
//! Supported IR vocabulary:
//!
//! - `Const(Bool, Int8..Int64, UInt8..UInt64)`.
//! - `BinaryOp::{Add, Eq, Gt, GtEq, Lt, LtEq, NotEq}`:
//!   `Sub`/`Mul`/`Div`/`Mod` are feature-gap follow-ups.
//! - `UnaryOp::{Neg, Not}`.
//! - `Call`: direct calls to functions declared in the same
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
//!   lowered through `koja-ir::lower_program`.
//! - [`compile_script`] / [`emit_script_llvm_ir`] for script-mode
//!   source lowered through `koja-ir::lower_script`.
//!
//! `compile_*` writes a native object file at the requested path.
//! Linking lives in `koja-driver`.
//!
//! # Module layout
//!
//! - [`ctx`]: [`ctx::EmitContext`] bundle (inkwell context + module +
//!   builder + per-emission counters + per-function slot table),
//!   the value threaded through every emit operation.
//! - [`layout`]: type-layout registry + host `TargetData` plus the
//!   pre-emit submodules (`layout::structs`, `layout::enums`) that
//!   mint LLVM types from sealed IR decls. Held as
//!   `EmitContext::layouts`.
//! - [`emit`]: IR-instruction-to-LLVM-instruction layer.
//!   `mod.rs` (block seams + lookups), `instruction.rs` (dispatch +
//!   const + call), `ops.rs` (binary + unary). Type creation lives
//!   in [`layout`].
//! - [`function`]: non-entry function declare + define +
//!   param/block seeding.
//! - [`main_wrapper`]: `i64 main()` synthesis + auto-print + the
//!   `__koja_app_name` global. **All temporary scaffolding**. This
//!   file is the deletion target when `IO.puts` lands.
//! - [`object`]: native `.o` emission via inkwell's `TargetMachine`.
//! - [`program`] / [`script`]: orchestrators for the two IR shapes.
//! - [`types`]: `IRType` -> inkwell `IntType` mapping.

mod constant_pool;
mod ctx;
mod debug;
mod emit;
mod error;
mod function;
mod intrinsics;
mod layout;
mod main_wrapper;
mod object;
mod program;
mod runtime;
mod script;
mod types;

pub use error::LlvmError;

use std::path::Path;

use inkwell::OptimizationLevel;
use inkwell::context::Context;
use koja_ir::{IRProgram, IRScript};

use crate::ctx::EmitContext;

/// Codegen knobs for the `compile_*` entry points. Kept inkwell-free
/// so the driver API stays decoupled from LLVM types, and a struct
/// (rather than positional flags) so future additions (debug-info
/// emission, `--target=<triple>`) land as new fields without churning
/// the signatures.
#[derive(Clone, Copy, Debug, Default)]
pub struct CompileOptions {
    /// Engage the LLVM optimization pipeline (`-O3`). Off keeps `-O0`.
    pub release: bool,
}

impl CompileOptions {
    fn opt_level(self) -> OptimizationLevel {
        if self.release {
            OptimizationLevel::Aggressive
        } else {
            OptimizationLevel::None
        }
    }
}

/// Compile a sealed [`IRProgram`] to a native object file at
/// `output`. `app_name` is embedded as the runtime's
/// `__koja_app_name` global (panic-backtrace label). Convention is
/// the binary's stem. Caller links the object into an executable.
pub fn compile_program(
    program: &IRProgram,
    app_name: &str,
    output: &Path,
    options: &CompileOptions,
) -> Result<(), LlvmError> {
    let context = Context::create();
    let ctx = EmitContext::new(&context, app_name, true);
    program::compile_program(&ctx, program, app_name)?;
    ctx.finalize_debug_info();
    object::emit_object_file(&ctx.module, output, options.opt_level())
}

/// Compile a sealed [`IRProgram`] and return its LLVM IR text, for
/// snapshot-style coverage in `tests/program.rs`. No linking, no
/// subprocess.
pub fn emit_llvm_ir(program: &IRProgram, app_name: &str) -> Result<String, LlvmError> {
    let context = Context::create();
    let ctx = EmitContext::new(&context, app_name, false);
    program::compile_program(&ctx, program, app_name)?;
    Ok(ctx.module.print_to_string().to_string())
}

/// Counterpart to [`compile_program`] for script-mode sources.
pub fn compile_script(
    script: &IRScript,
    app_name: &str,
    output: &Path,
    options: &CompileOptions,
) -> Result<(), LlvmError> {
    let context = Context::create();
    let ctx = EmitContext::new(&context, app_name, true);
    script::compile_script(&ctx, script, app_name)?;
    ctx.finalize_debug_info();
    object::emit_object_file(&ctx.module, output, options.opt_level())
}

/// Counterpart to [`emit_llvm_ir`] for script-mode sources.
pub fn emit_script_llvm_ir(script: &IRScript, app_name: &str) -> Result<String, LlvmError> {
    let context = Context::create();
    let ctx = EmitContext::new(&context, app_name, false);
    script::compile_script(&ctx, script, app_name)?;
    Ok(ctx.module.print_to_string().to_string())
}
