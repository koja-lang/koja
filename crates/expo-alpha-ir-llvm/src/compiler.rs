//! `Compiler<'ctx>` — owns the LLVM context-borrowed module and
//! builder for one compilation, and walks a sealed [`IRProgram`] to
//! populate the module.
//!
//! Mirrors the lifetime / ownership pattern in `expo-codegen`'s
//! `Compiler::new` (a `Context` is created by the caller, references
//! to it thread through the rest of codegen) but trims the surface to
//! the minimum needed for the alpha slice: no debug context, no
//! generic monomorphization map, no closure / process plumbing.
//!
//! Per the northstar's "consumer-builds-its-own-indices" rule, the
//! backend indexes its functions by mangled name (via
//! [`IRFunction::mangled_name`]) — never by decomposing
//! `Identifier` internals. A `BTreeMap<String, FunctionValue<'ctx>>`
//! lands with the slice that introduces `Call` lowering; until then
//! the compiler is a straight walk of `package.functions.values()`.

use expo_alpha_ir::{IRFunction, IRProgram};
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
/// [`crate::compile_program`]). All other LLVM objects (module,
/// builder, function values) borrow from `'ctx`.
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
    /// `compile_program` / `emit_llvm_ir` entry points) is responsible
    /// for emitting / printing — this method is just access.
    pub(crate) fn module(&self) -> &Module<'ctx> {
        &self.module
    }

    /// Walk the program and populate the module: declare every
    /// function, then define each one's body. Two-pass shape mirrors
    /// v1 codegen so adding `Call` lowering only needs a mangled-name
    /// → `FunctionValue` map between the two passes.
    pub(crate) fn compile(&self, program: &IRProgram) -> Result<(), LlvmError> {
        let mut declared =
            Vec::with_capacity(program.packages.iter().map(|p| p.functions.len()).sum());
        for package in &program.packages {
            for function in package.functions.values() {
                declared.push((function, self.declare_function(program, function)?));
            }
        }
        for (function, llvm_function) in declared {
            self.define_function(function, llvm_function)?;
        }
        Ok(())
    }

    /// Declare an LLVM function for `function` and return the
    /// resulting `FunctionValue`. The slice rejects parameters and
    /// non-integer return types up-front so the rest of the lowering
    /// can assume an integer-returning, parameter-less signature.
    fn declare_function(
        &self,
        program: &IRProgram,
        function: &IRFunction,
    ) -> Result<FunctionValue<'ctx>, LlvmError> {
        let name = llvm_symbol_name(program, function);
        let signature = self.function_signature(function)?;
        Ok(self
            .module
            .add_function(&name, signature, Some(Linkage::External)))
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

    /// Define `function`'s body: emit its single basic block by
    /// walking instructions + terminator through [`crate::lower`].
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

/// LLVM symbol name for an [`IRFunction`]. The entry point gets the
/// host-runtime symbol; every other function gets its IR-provided
/// mangled name. The backend never decomposes [`Identifier`]
/// internals — the entry-point check goes through [`IRProgram::is_entry`]
/// and the symbol name through [`IRFunction::mangled_name`].
fn llvm_symbol_name(program: &IRProgram, function: &IRFunction) -> String {
    if program.is_entry(function) {
        ENTRY_SYMBOL.to_string()
    } else {
        function.mangled_name()
    }
}
