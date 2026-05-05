//! Orchestrator for one compilation: owns the LLVM context-borrowed
//! module + builder and walks a sealed [`IRProgram`] / [`IRScript`]
//! to populate it.
//!
//! [`Compiler::emit_as_main`] is the shared seam between project-mode
//! and script-mode. It always emits `i64 main()` and wraps the body's
//! value in a call to one of the runtime printers in
//! [`expo-runtime/src/alpha.rs`](../../expo-runtime/src/alpha.rs)
//! before returning 0. That auto-print wrapper is temporary
//! scaffolding — see the runtime module for the deletion plan.

use std::collections::BTreeMap;

use expo_alpha_ir::{
    IRBasicBlock, IRBlockId, IRFunction, IRProgram, IRScript, IRTerminator, IRType,
};
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::types::{BasicMetadataTypeEnum, FunctionType, IntType};
use inkwell::values::{FunctionValue, IntValue};

use crate::emit::{self, BlockMap, ValueMap};
use crate::error::LlvmError;
use crate::types::ir_int_type;

const ENTRY_SYMBOL: &str = "main";
const PRINT_INT_SYMBOL: &str = "__expo_alpha_print_i64";
const PRINT_BOOL_SYMBOL: &str = "__expo_alpha_print_bool";
const APP_NAME_SYMBOL: &str = "__expo_app_name";

pub(crate) struct Compiler<'ctx> {
    builder: Builder<'ctx>,
    context: &'ctx Context,
    module: Module<'ctx>,
}

impl<'ctx> Compiler<'ctx> {
    pub(crate) fn new(context: &'ctx Context) -> Self {
        Self {
            builder: context.create_builder(),
            context,
            module: context.create_module("expo_alpha_module"),
        }
    }

    pub(crate) fn module(&self) -> &Module<'ctx> {
        &self.module
    }

    /// Emit the `__expo_app_name` global, declare every non-entry
    /// function, emit the entry as `main`, then define each
    /// non-entry function's body.
    pub(crate) fn compile_program(
        &self,
        program: &IRProgram,
        app_name: &str,
    ) -> Result<(), LlvmError> {
        self.emit_app_name_global(app_name);
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

    /// Emit the `__expo_app_name` global, declare every helper,
    /// emit the script body as `main`, then define each helper's
    /// body.
    pub(crate) fn compile_script(
        &self,
        script: &IRScript,
        app_name: &str,
    ) -> Result<(), LlvmError> {
        self.emit_app_name_global(app_name);
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

    /// Emit `__expo_app_name` as a null-terminated C-string constant.
    /// The `expo-runtime` panic handler reads it for backtrace labels
    /// (declared there as `extern static [c_char; 0]`); every
    /// alpha-compiled binary defines it so the runtime archive links
    /// cleanly regardless of codegen-unit partitioning.
    fn emit_app_name_global(&self, app_name: &str) {
        let value = self.context.const_string(app_name.as_bytes(), true);
        let global = self
            .module
            .add_global(value.get_type(), None, APP_NAME_SYMBOL);
        global.set_initializer(&value);
        global.set_constant(true);
    }

    /// Emit `blocks` as the host `main` function: declare
    /// `i64 main()`, pre-create one inkwell `BasicBlock` per IR
    /// block, walk every IR block's instructions in order, and
    /// intercept the trailing-block's `Return` so we can insert the
    /// auto-print call before `ret i64 0`. Branch / cond-branch
    /// terminators are lowered to `br` instructions verbatim.
    ///
    /// Empty bodies are illegal (sealed IR guarantees at least one
    /// block), and the final IR block must end in `Return`. The seal
    /// pass admits other terminators for non-trailing blocks; only
    /// the entry function's last block carries the auto-print
    /// scaffolding.
    fn emit_as_main(&self, blocks: &[IRBasicBlock], return_type: &IRType) -> Result<(), LlvmError> {
        let i64_type = self.context.i64_type();
        let signature = i64_type.fn_type(&[], false);
        let function = self
            .module
            .add_function(ENTRY_SYMBOL, signature, Some(Linkage::External));
        let block_map = self.declare_blocks(function, blocks);
        let return_block_id = self.find_return_block(blocks)?;
        let return_block = blocks
            .iter()
            .find(|b| b.id == return_block_id)
            .expect("return block must exist in IR");

        let mut values: ValueMap<'ctx> = ValueMap::new();
        for block in blocks {
            let llvm_block = block_map[&block.id];
            self.builder.position_at_end(llvm_block);
            if block.id == return_block_id {
                let (next_values, terminator) = emit::emit_instructions(
                    self.context,
                    &self.builder,
                    &self.module,
                    block,
                    std::mem::take(&mut values),
                )?;
                values = next_values;
                let body_value = match terminator {
                    IRTerminator::Return { value: Some(id) } => emit::lookup(&values, *id)?,
                    IRTerminator::Return { value: None } => {
                        return Err(LlvmError::Codegen(
                            "alpha LLVM does not yet emit Unit-returning `main`".to_string(),
                        ));
                    }
                    other => {
                        unreachable!("main return-block must terminate in Return; got {other:?}")
                    }
                };
                self.emit_print_call(return_type, body_value)?;
                self.builder
                    .build_return(Some(&i64_type.const_int(0, false)))
                    .map(|_| ())
                    .map_err(|e| {
                        LlvmError::Codegen(format!("inkwell rejected build_return for main: {e}"))
                    })?;
            } else {
                emit::emit_block(
                    self.context,
                    &self.builder,
                    &self.module,
                    block,
                    &block_map,
                    &mut values,
                )?;
            }
        }
        // Suppress the unused-binding warning while keeping the
        // shape parallel to the helper paths.
        let _ = return_block;
        Ok(())
    }

    /// Pre-create one inkwell [`BasicBlock`] per IR block on
    /// `function`, returning the [`IRBlockId`] -> [`BasicBlock`]
    /// index emit consumes when lowering branch terminators.
    fn declare_blocks(
        &self,
        function: FunctionValue<'ctx>,
        blocks: &[IRBasicBlock],
    ) -> BlockMap<'ctx> {
        let mut block_map: BTreeMap<IRBlockId, BasicBlock<'ctx>> = BTreeMap::new();
        for block in blocks {
            let llvm_block = self.context.append_basic_block(function, &block.label);
            block_map.insert(block.id, llvm_block);
        }
        block_map
    }

    /// The `IRBlockId` of the unique block ending in `Return`. The
    /// auto-print wrapper around `main` patches in `ret i64 0` after
    /// executing the body, so we need to know which IR block carries
    /// the body's value before walking. Today's slice produces
    /// exactly one `Return`-terminated block per function (the
    /// merge block of an `if` / `unless` falls through to it via
    /// `Branch`), so a missing or duplicate `Return` is a lowering
    /// bug we surface as a codegen error.
    fn find_return_block(&self, blocks: &[IRBasicBlock]) -> Result<IRBlockId, LlvmError> {
        let mut found: Option<IRBlockId> = None;
        for block in blocks {
            if matches!(block.terminator, IRTerminator::Return { .. }) {
                if found.is_some() {
                    return Err(LlvmError::Codegen(
                        "alpha LLVM expects exactly one Return-terminated block in `main`"
                            .to_string(),
                    ));
                }
                found = Some(block.id);
            }
        }
        found.ok_or_else(|| {
            LlvmError::Codegen(
                "alpha LLVM expects at least one Return-terminated block in `main`".to_string(),
            )
        })
    }

    /// Pick the runtime printer for `return_type`, extend `body_value`
    /// to `i64` (sign-extended for signed widths, zero-extended for
    /// unsigned widths and `Bool`), and emit the call.
    fn emit_print_call(
        &self,
        return_type: &IRType,
        body_value: IntValue<'ctx>,
    ) -> Result<(), LlvmError> {
        let i64_type = self.context.i64_type();
        let (printer_symbol, argument) = match return_type {
            IRType::Bool => (PRINT_BOOL_SYMBOL, self.zext_to_i64(body_value)?),
            IRType::Int8 | IRType::Int16 | IRType::Int32 => {
                (PRINT_INT_SYMBOL, self.sext_to_i64(body_value)?)
            }
            IRType::Int64 | IRType::UInt64 => (PRINT_INT_SYMBOL, body_value),
            IRType::UInt8 | IRType::UInt16 | IRType::UInt32 => {
                (PRINT_INT_SYMBOL, self.zext_to_i64(body_value)?)
            }
            IRType::Unit => {
                return Err(LlvmError::Codegen(
                    "alpha LLVM does not yet emit Unit-typed main bodies".to_string(),
                ));
            }
        };
        let printer = self.declare_runtime_printer(printer_symbol, i64_type);
        self.builder
            .build_call(printer, &[argument.into()], "")
            .map(|_| ())
            .map_err(|e| LlvmError::Codegen(format!("inkwell rejected print call: {e}")))
    }

    fn declare_runtime_printer(
        &self,
        symbol: &str,
        argument_type: IntType<'ctx>,
    ) -> FunctionValue<'ctx> {
        if let Some(existing) = self.module.get_function(symbol) {
            return existing;
        }
        let signature = self
            .context
            .void_type()
            .fn_type(&[argument_type.into()], false);
        self.module
            .add_function(symbol, signature, Some(Linkage::External))
    }

    fn sext_to_i64(&self, value: IntValue<'ctx>) -> Result<IntValue<'ctx>, LlvmError> {
        self.builder
            .build_int_s_extend(value, self.context.i64_type(), "print_arg")
            .map_err(|e| LlvmError::Codegen(format!("inkwell rejected sext for print arg: {e}")))
    }

    fn zext_to_i64(&self, value: IntValue<'ctx>) -> Result<IntValue<'ctx>, LlvmError> {
        self.builder
            .build_int_z_extend(value, self.context.i64_type(), "print_arg")
            .map_err(|e| LlvmError::Codegen(format!("inkwell rejected zext for print arg: {e}")))
    }

    /// Declare an LLVM function for a non-entry [`IRFunction`] under
    /// its mangled [`expo_alpha_ir::IRSymbol`]. The signature mirrors
    /// the IR exactly: each [`expo_alpha_ir::IRFunctionParam::ty`]
    /// becomes an LLVM `iN` parameter, and the return type does the
    /// same. Non-integer types still surface as feature-gap
    /// diagnostics through [`ir_int_type`] until non-scalar lowering
    /// lands.
    fn declare_function(&self, function: &IRFunction) -> Result<FunctionValue<'ctx>, LlvmError> {
        let signature = self.function_signature(function)?;
        Ok(self.module.add_function(
            function.symbol.mangled(),
            signature,
            Some(Linkage::External),
        ))
    }

    fn function_signature(&self, function: &IRFunction) -> Result<FunctionType<'ctx>, LlvmError> {
        let mut param_types: Vec<BasicMetadataTypeEnum<'ctx>> =
            Vec::with_capacity(function.params.len());
        for param in &function.params {
            param_types.push(ir_int_type(self.context, &param.ty)?.into());
        }
        let return_int = ir_int_type(self.context, &function.return_type)?;
        Ok(return_int.fn_type(&param_types, false))
    }

    /// Define a non-entry function's body. Helpers keep the natural
    /// `Return`-to-`ret` emission via [`emit::emit_block`] — only
    /// `main` gets the auto-print wrapper. Pre-creates one inkwell
    /// `BasicBlock` per IR block so `Branch` / `CondBranch`
    /// terminators can resolve to a real `inkwell::BasicBlock`. The
    /// body's `ValueMap` is seeded with each
    /// [`expo_alpha_ir::IRFunctionParam`] bound to the matching
    /// `function.get_nth_param(i)` LLVM value before walking the
    /// entry block.
    fn define_function(
        &self,
        function: &IRFunction,
        llvm_function: FunctionValue<'ctx>,
    ) -> Result<(), LlvmError> {
        let block_map = self.declare_blocks(llvm_function, &function.blocks);
        let mut values = self.seed_params(function, llvm_function)?;
        for block in &function.blocks {
            let llvm_block = block_map[&block.id];
            self.builder.position_at_end(llvm_block);
            emit::emit_block(
                self.context,
                &self.builder,
                &self.module,
                block,
                &block_map,
                &mut values,
            )?;
        }
        Ok(())
    }

    /// Seed a fresh [`ValueMap`] with each parameter's LLVM value,
    /// keyed by the [`expo_alpha_ir::IRFunctionParam::id`] that body
    /// lowering uses. Inkwell's `get_nth_param` panics on
    /// out-of-bounds; the IR seal guarantees `params.len()` matches
    /// the LLVM function's arity.
    fn seed_params(
        &self,
        function: &IRFunction,
        llvm_function: FunctionValue<'ctx>,
    ) -> Result<ValueMap<'ctx>, LlvmError> {
        let mut seed = ValueMap::new();
        for (index, param) in function.params.iter().enumerate() {
            let llvm_param = llvm_function
                .get_nth_param(index as u32)
                .unwrap_or_else(|| {
                    panic!(
                        "alpha LLVM emit: missing LLVM param #{index} on `{}` — \
                         signature/IR arity mismatch",
                        function.symbol,
                    )
                });
            seed.insert(param.id, llvm_param.into_int_value());
        }
        Ok(seed)
    }
}
