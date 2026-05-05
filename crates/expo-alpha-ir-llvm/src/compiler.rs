//! `Compiler<'ctx>` — owns the LLVM context-borrowed module and
//! builder for one compilation, and walks a sealed
//! [`expo_alpha_ir::IRProgram`] or [`expo_alpha_ir::IRScript`] to
//! populate the module.
//!
//! Mirrors the lifetime / ownership pattern in `expo-codegen`'s
//! `Compiler::new` (a `Context` is created by the caller, references
//! to it thread through the rest of codegen) but trims the surface to
//! the minimum needed for the alpha slice: no debug context, no
//! generic monomorphization map, no closure / process plumbing.
//!
//! The entry function (project-mode) and the implicit script body
//! (script-mode) share a single seam — [`Compiler::emit_as_main`] —
//! that emits one `main` LLVM function from a `&[IRBasicBlock]` plus
//! its `IRType` return type. The two `compile_*` entry points differ
//! only in whether they discover that "main body" by looking up
//! `program.entry_function()` or by reading `script.blocks` /
//! `script.return_type` directly.
//!
//! Per the northstar's "consumer-builds-its-own-indices" rule, this
//! backend indexes its functions by mangled name (via
//! [`IRFunction::mangled_name`]) — never by decomposing
//! `Identifier` internals. A `BTreeMap<String, FunctionValue<'ctx>>`
//! lands with the slice that introduces `Call` lowering; until then
//! the compiler is a straight walk of `package.functions.values()`.

use expo_alpha_ir::{IRBasicBlock, IRFunction, IRProgram, IRScript, IRType};
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::types::FunctionType;
use inkwell::values::FunctionValue;

use crate::error::LlvmError;
use crate::lower;
use crate::types::ir_int_type;

/// Host-runtime symbol the entry function is exported under. Unix
/// follows the C-runtime `main` contract; ports to other targets
/// adjust this constant.
const ENTRY_SYMBOL: &str = "main";

/// Holds the LLVM state for one compilation. The `Context` outlives
/// the compiler and is supplied by the caller (typically
/// [`crate::compile_program`] or [`crate::compile_script`]). All
/// other LLVM objects (module, builder, function values) borrow
/// from `'ctx`.
pub(crate) struct Compiler<'ctx> {
    builder: Builder<'ctx>,
    context: &'ctx Context,
    module: Module<'ctx>,
}

impl<'ctx> Compiler<'ctx> {
    /// Construct a fresh compiler. Module name is fixed to
    /// `expo_alpha_module`; we don't yet thread the source filename
    /// or app name through.
    pub(crate) fn new(context: &'ctx Context) -> Self {
        Self {
            builder: context.create_builder(),
            context,
            module: context.create_module("expo_alpha_module"),
        }
    }

    /// Borrow the populated LLVM module. The caller (the public
    /// `compile_*` / `emit_*_llvm_ir` entry points) is responsible
    /// for emitting / printing — this method is just access.
    pub(crate) fn module(&self) -> &Module<'ctx> {
        &self.module
    }

    /// Walk `program` and populate the module:
    ///
    /// 1. Declare every non-entry function under its mangled name so
    ///    future `Call` lowering can resolve targets in any direction
    ///    (caller → callee, mutual recursion).
    /// 2. Emit the entry function's body directly as `main` via
    ///    [`Self::emit_as_main`] — no wrapper indirection.
    /// 3. Define each non-entry function's body.
    pub(crate) fn compile_program(&self, program: &IRProgram) -> Result<(), LlvmError> {
        let mut declared =
            Vec::with_capacity(program.packages.iter().map(|p| p.functions.len()).sum());
        for package in &program.packages {
            for function in package.functions.values() {
                if program.is_entry(function) {
                    continue;
                }
                declared.push((function, self.declare_function(function)?));
            }
        }
        let entry = program.entry_function();
        self.emit_as_main(&entry.blocks, &entry.return_type)?;
        for (function, llvm_function) in declared {
            self.define_function(function, llvm_function)?;
        }
        Ok(())
    }

    /// Walk `script` and populate the module:
    ///
    /// 1. Declare every helper function in `script.packages` under
    ///    its mangled name (none of them is the entry — the script
    ///    body is).
    /// 2. Emit the script body directly as `main` via
    ///    [`Self::emit_as_main`].
    /// 3. Define each helper function's body.
    pub(crate) fn compile_script(&self, script: &IRScript) -> Result<(), LlvmError> {
        let mut declared =
            Vec::with_capacity(script.packages.iter().map(|p| p.functions.len()).sum());
        for package in &script.packages {
            for function in package.functions.values() {
                declared.push((function, self.declare_function(function)?));
            }
        }
        self.emit_as_main(&script.blocks, &script.return_type)?;
        for (function, llvm_function) in declared {
            self.define_function(function, llvm_function)?;
        }
        Ok(())
    }

    /// The shared "emit a `&[IRBasicBlock]` as the host `main`
    /// function" seam. Builds the signature from `return_type`,
    /// declares `main` as an external symbol, and walks the single
    /// supported entry block through [`crate::lower::emit_block`].
    /// Multi-block bodies surface as a feature-gap diagnostic until
    /// branch terminators land.
    fn emit_as_main(&self, blocks: &[IRBasicBlock], return_type: &IRType) -> Result<(), LlvmError> {
        if blocks.len() != 1 {
            return Err(LlvmError::Codegen(format!(
                "alpha LLVM does not yet emit multi-block `main` bodies (got {} blocks)",
                blocks.len(),
            )));
        }
        let return_int = ir_int_type(self.context, return_type)?;
        let signature = return_int.fn_type(&[], false);
        let function = self
            .module
            .add_function(ENTRY_SYMBOL, signature, Some(Linkage::External));
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);
        lower::emit_block(self.context, &self.builder, &blocks[0])
    }

    /// Declare an LLVM function for a non-entry [`IRFunction`] under
    /// its mangled name and return the resulting [`FunctionValue`].
    /// The slice rejects parameters and non-integer return types
    /// up-front so the rest of the lowering can assume an
    /// integer-returning, parameter-less signature.
    fn declare_function(&self, function: &IRFunction) -> Result<FunctionValue<'ctx>, LlvmError> {
        let signature = self.function_signature(function)?;
        Ok(self
            .module
            .add_function(&function.mangled_name(), signature, Some(Linkage::External)))
    }

    fn function_signature(&self, function: &IRFunction) -> Result<FunctionType<'ctx>, LlvmError> {
        if !function.params.is_empty() {
            return Err(LlvmError::Codegen(format!(
                "alpha LLVM does not yet emit function parameters (`{}` has {} params)",
                function.mangled_name(),
                function.params.len(),
            )));
        }
        let return_int = ir_int_type(self.context, &function.return_type)?;
        Ok(return_int.fn_type(&[], false))
    }

    /// Define `function`'s body into the previously-declared
    /// [`FunctionValue`]: emit its single basic block by walking
    /// instructions + terminator through [`crate::lower`].
    fn define_function(
        &self,
        function: &IRFunction,
        llvm_function: FunctionValue<'ctx>,
    ) -> Result<(), LlvmError> {
        if function.blocks.len() != 1 {
            return Err(LlvmError::Codegen(format!(
                "alpha LLVM does not yet emit multi-block functions (`{}` has {} blocks)",
                function.mangled_name(),
                function.blocks.len(),
            )));
        }
        let entry = self.context.append_basic_block(llvm_function, "entry");
        self.builder.position_at_end(entry);
        lower::emit_block(self.context, &self.builder, &function.blocks[0])
    }
}
