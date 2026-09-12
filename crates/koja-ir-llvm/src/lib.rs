//! LLVM backend for sealed [`koja_ir::IRProgram`]s and
//! [`koja_ir::IRScript`]s: peer to
//! [`koja-ir-eval`](../koja_ir_eval/index.html) but
//! emitting native object code via [`inkwell`] instead of
//! interpreting in-process.
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
//! [`program`] and [`script`] orchestrate a compile. [`layout`] mints
//! LLVM types from sealed IR decls, [`function`] declares and defines
//! functions, [`emit`] lowers instructions, [`intrinsics`] synthesizes
//! `@intrinsic` bodies, and [`runtime`] declares the `koja-runtime`
//! externs they call.

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
mod reductions;
mod runtime;
mod script;
mod target;
mod types;

pub use error::LlvmError;
pub use target::TargetCpu;

use std::path::Path;

use inkwell::OptimizationLevel;
use inkwell::context::Context;
use inkwell::module::Module;
use koja_ir::{IRProgram, IRScript};

use crate::ctx::EmitContext;
use crate::target::TargetSpec;

/// Codegen knobs for the `compile_*` entry points. Kept inkwell-free
/// so the driver API stays decoupled from LLVM types, and a struct
/// (rather than positional flags) so future additions (debug-info
/// emission, `--target=<triple>`) land as new fields without churning
/// the signatures.
#[derive(Clone, Copy, Debug, Default)]
pub struct CompileOptions {
    /// Engage the LLVM optimization pipeline (`-O3`). Off keeps `-O0`.
    pub release: bool,
    /// CPU the emitted code may assume. Defaults to a portable
    /// baseline for the build architecture.
    pub target_cpu: TargetCpu,
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
    let ctx = emit_context(&context, app_name, true, options)?;
    program::compile_program(&ctx, program, app_name)?;
    ctx.finalize_debug_info();
    verify_module(&ctx.module)?;
    object::emit_object_file(&ctx, output, options.opt_level())
}

/// Compile a sealed [`IRProgram`] and return its LLVM IR text, for
/// snapshot-style coverage in `tests/program.rs`. No linking, no
/// subprocess.
pub fn emit_llvm_ir(program: &IRProgram, app_name: &str) -> Result<String, LlvmError> {
    let context = Context::create();
    let ctx = emit_context(&context, app_name, false, &CompileOptions::default())?;
    program::compile_program(&ctx, program, app_name)?;
    verify_module(&ctx.module)?;
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
    let ctx = emit_context(&context, app_name, true, options)?;
    script::compile_script(&ctx, script, app_name)?;
    ctx.finalize_debug_info();
    verify_module(&ctx.module)?;
    object::emit_object_file(&ctx, output, options.opt_level())
}

/// Counterpart to [`emit_llvm_ir`] for script-mode sources.
pub fn emit_script_llvm_ir(script: &IRScript, app_name: &str) -> Result<String, LlvmError> {
    let context = Context::create();
    let ctx = emit_context(&context, app_name, false, &CompileOptions::default())?;
    script::compile_script(&ctx, script, app_name)?;
    verify_module(&ctx.module)?;
    Ok(ctx.module.print_to_string().to_string())
}

/// Build the emit context around the compile's single target machine.
fn emit_context<'ctx>(
    context: &'ctx Context,
    app_name: &str,
    emit_debug_info: bool,
    options: &CompileOptions,
) -> Result<EmitContext<'ctx>, LlvmError> {
    let target_machine = TargetSpec::new(options.target_cpu).target_machine(options.opt_level())?;
    Ok(EmitContext::new(
        context,
        app_name,
        emit_debug_info,
        target_machine,
    ))
}

/// Run LLVM's module verifier on the freshly built module. Any
/// failure is an internal compiler error in the emit layer, caught
/// here before it turns into miscompiled machine code. The offending
/// module is dumped beside the temp dir for postmortem debugging.
fn verify_module(module: &Module<'_>) -> Result<(), LlvmError> {
    module.verify().map_err(|message| {
        let dump = std::env::temp_dir().join("koja-verify-failure.ll");
        let dumped = module.print_to_file(&dump).is_ok();
        let suffix = if dumped {
            format!(" (module dumped to {})", dump.display())
        } else {
            String::new()
        };
        LlvmError::Codegen(format!("module verification failed{suffix}: {message}"))
    })
}
