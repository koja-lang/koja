//! Tree-walking interpreter for [`expo_ir::IRProgram`].
//!
//! Implements [`expo_ir::Backend`] by walking the IR's basic blocks
//! and dispatching each [`expo_ir::IRInstruction`] to a Rust handler
//! that produces or stores a [`crate::Value`]. The full instruction
//! coverage matrix lives in [`Interp::execute_instruction`]; coverage
//! gaps return [`RuntimeError::Unsupported`] with a descriptive
//! payload.
//!
//! Helpers that don't recurse through `call` live in sibling modules:
//!
//! - [`crate::aggregates`] -- struct/enum/variant payload construction
//!   and field-walk projection.
//! - [`crate::control`] -- block lookup, terminator interpretation,
//!   parameter binding, and the [`crate::control::ControlFlow`] signal.
//! - [`crate::ops`] -- pure binary/unary operator evaluation.
//! - [`crate::frame::Frame::materialize`] -- operand resolution.

use std::sync::Arc;

use expo_ir::resolved::strings::ResolvedConcatKind;
use expo_ir::{
    Backend, FunctionIdentifier, IRBasicBlock, IRBlockId, IRFunction, IRFunctionKind,
    IRInstruction, IROperand, IRParam, IRProgram, IRValueId,
};
use expo_typecheck::context::TypeContext;

use crate::aggregates::{build_enum_value, build_struct_value, walk_field_steps};
use crate::binary::construct_binary;
use crate::concat::{concat_binaries, concat_strings};
use crate::constants::materialize_ir_constant_value;
use crate::control::{ControlFlow, bind_params, block_by_id, follow_terminator};
use crate::error::RuntimeError;
use crate::format::format_string;
use crate::frame::Frame;
use crate::ops::{eval_binary_op, eval_unary_op};
use crate::pattern::{pattern_bind_from_ptr, pattern_literal_eq, pattern_project_struct_field};
use crate::value::Value;

pub struct Interp {
    /// Materialized [`expo_ir::IRProgram::constants`], indexed by
    /// [`expo_ir::IRConstId`]. Built once at [`Interp::new`].
    pub constants: Vec<Value>,
    pub program: Arc<IRProgram>,
    pub type_ctx: Arc<TypeContext>,
}

impl Backend for Interp {
    type Value = Value;
    type Error = RuntimeError;

    fn new(program: Arc<IRProgram>, type_ctx: Arc<TypeContext>) -> Result<Self, Self::Error> {
        program.validate()?;
        let constants = program
            .constants
            .iter()
            .map(|entry| materialize_ir_constant_value(&entry.value))
            .collect();
        Ok(Self {
            constants,
            program,
            type_ctx,
        })
    }

    fn call(
        &mut self,
        callee: &FunctionIdentifier,
        args: Vec<Self::Value>,
    ) -> Result<Self::Value, Self::Error> {
        let function = self
            .program
            .functions
            .get(callee)
            .ok_or_else(|| RuntimeError::UnknownCallee(callee.clone()))?
            .clone();
        self.dispatch_function(&function, args)
    }

    fn format_value(&self, value: &Self::Value) -> String {
        value.to_string()
    }
}

impl Interp {
    fn dispatch_function(
        &mut self,
        function: &IRFunction,
        args: Vec<Value>,
    ) -> Result<Value, RuntimeError> {
        match &function.kind {
            IRFunctionKind::Extern { .. } => {
                Err(RuntimeError::ExternNotSupported(function.mangled.clone()))
            }
            IRFunctionKind::Free { meta, blocks, .. }
            | IRFunctionKind::Method { meta, blocks, .. } => {
                self.execute_blocks(meta.params.as_slice(), blocks, args)
            }
            IRFunctionKind::Intrinsic { .. } => Err(RuntimeError::Unsupported(format!(
                "intrinsic dispatch not yet implemented for `{}`",
                function.mangled
            ))),
            IRFunctionKind::MainEntry => Err(RuntimeError::Unsupported(format!(
                "MainEntry kind cannot be called directly: `{}`",
                function.mangled
            ))),
            IRFunctionKind::Thunk { wraps } => self.call(&wraps.clone(), args),
        }
    }

    fn execute_blocks(
        &mut self,
        params: &[IRParam],
        blocks: &[IRBasicBlock],
        args: Vec<Value>,
    ) -> Result<Value, RuntimeError> {
        let mut frame = Frame::new();
        bind_params(&mut frame, params, args);
        let entry = blocks
            .first()
            .ok_or_else(|| RuntimeError::Unsupported("function has no blocks".into()))?;
        let mut current = entry;
        let mut previous: Option<IRBlockId> = None;
        loop {
            for instruction in &current.instructions {
                self.execute_instruction(&mut frame, previous, instruction)?;
            }
            match follow_terminator(&frame, &current.terminator)? {
                ControlFlow::Goto(target) => {
                    previous = Some(current.id);
                    current = block_by_id(blocks, target)?;
                }
                ControlFlow::Return(value) => return Ok(value),
            }
        }
    }

    fn execute_instruction(
        &mut self,
        frame: &mut Frame,
        previous: Option<IRBlockId>,
        instruction: &IRInstruction,
    ) -> Result<(), RuntimeError> {
        match instruction {
            IRInstruction::BinaryConstruct {
                dest,
                layout,
                segments,
            } => {
                let value = construct_binary(frame, layout, segments)?;
                frame.values.insert(*dest, value);
                Ok(())
            }
            IRInstruction::BinaryOp { dest, op, lhs, rhs } => {
                let lhs_value = frame.materialize(lhs)?;
                let rhs_value = frame.materialize(rhs)?;
                let result = eval_binary_op(op, lhs_value, rhs_value)?;
                frame.values.insert(*dest, result);
                Ok(())
            }
            IRInstruction::Call {
                dest,
                mangled,
                args,
                ..
            } => self.execute_call(frame, *dest, mangled, args, None),
            IRInstruction::Concat { dest, kind, parts } => {
                let materialized: Result<Vec<Value>, _> =
                    parts.iter().map(|p| frame.materialize(p)).collect();
                let result = match kind {
                    ResolvedConcatKind::Binary => concat_binaries(materialized?)?,
                    ResolvedConcatKind::String => concat_strings(materialized?)?,
                };
                frame.values.insert(*dest, result);
                Ok(())
            }
            IRInstruction::EnumConstruct {
                dest,
                mangled,
                tag,
                variant,
                payload,
                ..
            } => {
                let value = build_enum_value(frame, mangled, *tag, variant, payload)?;
                frame.values.insert(*dest, value);
                Ok(())
            }
            IRInstruction::FieldChain {
                dest,
                base_name,
                steps,
                ..
            } => {
                let root = frame
                    .locals
                    .get(base_name)
                    .cloned()
                    .ok_or_else(|| RuntimeError::UndefinedLocal(base_name.clone()))?;
                let value = walk_field_steps(root, steps)?;
                frame.values.insert(*dest, value);
                Ok(())
            }
            IRInstruction::FieldLoad { dest, base, step } => {
                let value = frame.materialize(base)?;
                let projected = walk_field_steps(value, std::slice::from_ref(step))?;
                frame.values.insert(*dest, projected);
                Ok(())
            }
            IRInstruction::LoadConst { dest, id, .. } => {
                let value = self
                    .constants
                    .get(id.0 as usize)
                    .ok_or_else(|| {
                        RuntimeError::Unsupported(format!(
                            "LoadConst: out-of-range constant id {}",
                            id.0
                        ))
                    })?
                    .clone();
                frame.values.insert(*dest, value);
                Ok(())
            }
            IRInstruction::LoadLocal { dest, name, .. } => {
                let value = frame
                    .locals
                    .get(name)
                    .cloned()
                    .ok_or_else(|| RuntimeError::UndefinedLocal(name.clone()))?;
                frame.values.insert(*dest, value);
                Ok(())
            }
            IRInstruction::MatchSubject { dest, value, .. } => {
                let subject = frame.materialize(value)?;
                frame.values.insert(*dest, subject);
                Ok(())
            }
            IRInstruction::MethodCall {
                dest,
                mangled,
                receiver,
                args,
                ..
            } => self.execute_call(frame, *dest, mangled, args, Some(receiver)),
            IRInstruction::PatternBindFromPtr {
                name, source_ptr, ..
            } => pattern_bind_from_ptr(frame, name, source_ptr),
            IRInstruction::PatternLiteralEq {
                dest,
                subject_ptr,
                lit,
                ..
            } => {
                let value = pattern_literal_eq(frame, subject_ptr, lit)?;
                frame.values.insert(*dest, value);
                Ok(())
            }
            IRInstruction::PatternProjectStructField {
                dest,
                subject_ptr,
                field_index,
                ..
            } => {
                let value = pattern_project_struct_field(frame, subject_ptr, *field_index)?;
                frame.values.insert(*dest, value);
                Ok(())
            }
            IRInstruction::Phi {
                dest, incomings, ..
            } => {
                let prev = previous.ok_or_else(|| {
                    RuntimeError::Unsupported(
                        "Phi instruction in entry block has no predecessor".into(),
                    )
                })?;
                let (_, operand) = incomings
                    .iter()
                    .find(|(block, _)| *block == prev)
                    .ok_or_else(|| {
                        RuntimeError::Unsupported(format!(
                            "Phi has no incoming for predecessor {prev:?}"
                        ))
                    })?;
                let value = frame.materialize(operand)?;
                frame.values.insert(*dest, value);
                Ok(())
            }
            IRInstruction::PopTypeSubst { .. } | IRInstruction::PushTypeSubst { .. } => Ok(()),
            IRInstruction::StoreLocal { name, value, .. } => {
                let stored = frame.materialize(value)?;
                frame.locals.insert(name.clone(), stored);
                Ok(())
            }
            IRInstruction::StringFormat { dest, parts } => {
                let value = format_string(frame, parts)?;
                frame.values.insert(*dest, value);
                Ok(())
            }
            IRInstruction::StructConstruct {
                dest,
                mangled,
                fields,
                ..
            } => {
                let value = build_struct_value(frame, mangled, fields)?;
                frame.values.insert(*dest, value);
                Ok(())
            }
            IRInstruction::UnaryOp { dest, op, operand } => {
                let inner = frame.materialize(operand)?;
                let result = eval_unary_op(op, inner)?;
                frame.values.insert(*dest, result);
                Ok(())
            }
            other => Err(RuntimeError::Unsupported(format!(
                "instruction not yet implemented: {other:?}"
            ))),
        }
    }

    fn execute_call(
        &mut self,
        frame: &mut Frame,
        dest: IRValueId,
        mangled: &FunctionIdentifier,
        args: &[IROperand],
        receiver: Option<&IROperand>,
    ) -> Result<(), RuntimeError> {
        let mut call_args = Vec::with_capacity(args.len() + 1);
        if let Some(receiver) = receiver {
            call_args.push(frame.materialize(receiver)?);
        }
        for arg in args {
            call_args.push(frame.materialize(arg)?);
        }
        let result = self.call(mangled, call_args)?;
        frame.values.insert(dest, result);
        Ok(())
    }
}
