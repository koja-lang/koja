//! Tree-walking interpreter over a sealed [`IRProgram`] or
//! [`IRScript`]. The walker is parameterized over a [`CallResolver`]
//! so both IR shapes share the per-instruction execution / frame
//! management / terminator dispatch code; only callee and per-id
//! block lookup differ.
//!
//! `Call` instructions chain through [`execute_function`]: evaluate
//! the args in the current frame, resolve the callee, seed a fresh
//! frame with param `ValueId`s bound to arg values, recurse. Call
//! depth is uncapped — pathological recursion propagates as a native
//! Rust stack overflow.
//!
//! Multi-block bodies are driven by [`execute_blocks`]: walk the
//! current block's instructions, then dispatch on the terminator —
//! `Return` exits the function with a value, `Branch` and
//! `CondBranch` move the cursor to the named target block. The frame
//! is shared across every block in a single function (per
//! `IRFunction` boundary), mirroring the IR's "value ids are
//! function-scoped" contract.
//!
//! Operator math (`apply_binary_op`, `apply_unary_op`) lives in the
//! sibling [`crate::ops`] module — pure functions over already-resolved
//! [`Value`] operands. This file owns the IR walking, frame
//! management, and resolver wiring; nothing else.

use std::collections::BTreeMap;

use expo_alpha_ir::{
    ConstValue, IRBasicBlock, IRBlockId, IRFunction, IRInstruction, IRProgram, IRScript,
    IRTerminator, ValueId,
};

use crate::error::RuntimeError;
use crate::ops::{apply_binary_op, apply_unary_op};
use crate::value::Value;

pub struct Interpreter;

impl Interpreter {
    /// Execute the project-mode entry function and return the value
    /// it produces (or [`Value::Unit`] if the entry returns nothing).
    pub fn run_program(program: IRProgram) -> Result<Value, RuntimeError> {
        let entry = program.entry_function();
        execute_function(entry, Vec::new(), &program)
    }

    /// Execute the script-mode implicit body and return its trailing
    /// value (or [`Value::Unit`] for an empty / non-expression-trailing
    /// body).
    pub fn run_script(script: IRScript) -> Result<Value, RuntimeError> {
        let mut frame: BTreeMap<ValueId, Value> = BTreeMap::new();
        execute_blocks(&script.blocks, &mut frame, &script)
    }
}

/// Dereferences a `Call` callee to its target [`IRFunction`] by
/// mangled symbol. Implemented by both [`IRProgram`] and [`IRScript`]
/// so one walker drives either; the IR's stable
/// [`expo_alpha_ir::IRSymbol`] is the only handle the interpreter
/// needs — no AST [`expo_ast`] types here.
trait CallResolver {
    fn resolve(&self, mangled: &str) -> Option<&IRFunction>;
}

impl CallResolver for IRProgram {
    fn resolve(&self, mangled: &str) -> Option<&IRFunction> {
        self.function(mangled)
    }
}

impl CallResolver for IRScript {
    fn resolve(&self, mangled: &str) -> Option<&IRFunction> {
        self.function(mangled)
    }
}

/// Run `function` in a fresh frame with `args` positionally bound to
/// its param `ValueId`s. Multi-block bodies dispatch through
/// [`execute_blocks`].
fn execute_function<R: CallResolver>(
    function: &IRFunction,
    args: Vec<Value>,
    resolver: &R,
) -> Result<Value, RuntimeError> {
    debug_assert_eq!(
        function.params.len(),
        args.len(),
        "arity mismatch calling `{}`: {} params vs {} args (typecheck invariant)",
        function.symbol,
        function.params.len(),
        args.len(),
    );
    let mut frame: BTreeMap<ValueId, Value> = BTreeMap::new();
    for (param, value) in function.params.iter().zip(args.into_iter()) {
        frame.insert(param.id, value);
    }

    execute_blocks(&function.blocks, &mut frame, resolver)
}

/// Drive a function (or script body) starting at `blocks[0]` until a
/// `Return` terminator exits with a value. `Branch` / `CondBranch`
/// move the cursor; the frame stays alive across every block in the
/// function. Unknown branch targets panic per the seal contract — by
/// the time the IR reaches an interpreter run those have all been
/// validated.
fn execute_blocks<R: CallResolver>(
    blocks: &[IRBasicBlock],
    frame: &mut BTreeMap<ValueId, Value>,
    resolver: &R,
) -> Result<Value, RuntimeError> {
    let mut current = blocks
        .first()
        .expect("sealed function has at least one basic block")
        .id;
    loop {
        let block = find_block(blocks, current);
        for instruction in &block.instructions {
            execute_instruction(instruction, frame, resolver)?;
        }
        match &block.terminator {
            IRTerminator::Branch(target) => current = *target,
            IRTerminator::CondBranch {
                cond,
                then_block,
                else_block,
            } => {
                let cond_value = lookup(frame, *cond)?;
                let Value::Bool(b) = cond_value else {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!("cond_branch expects a Bool condition; got {cond_value}",),
                    });
                };
                current = if b { *then_block } else { *else_block };
            }
            IRTerminator::Return { value: None } => return Ok(Value::Unit),
            IRTerminator::Return { value: Some(id) } => return lookup(frame, *id),
        }
    }
}

fn find_block(blocks: &[IRBasicBlock], id: IRBlockId) -> &IRBasicBlock {
    blocks
        .iter()
        .find(|b| b.id == id)
        .unwrap_or_else(|| panic!("interpreter: block `{id}` missing — seal invariant violation"))
}

fn execute_instruction<R: CallResolver>(
    instruction: &IRInstruction,
    frame: &mut BTreeMap<ValueId, Value>,
    resolver: &R,
) -> Result<(), RuntimeError> {
    match instruction {
        IRInstruction::BinaryOp { dest, lhs, op, rhs } => {
            let lhs_value = lookup(frame, *lhs)?;
            let rhs_value = lookup(frame, *rhs)?;
            let result = apply_binary_op(*op, lhs_value, rhs_value)?;
            frame.insert(*dest, result);
            Ok(())
        }
        IRInstruction::Call { dest, callee, args } => {
            let mut arg_values = Vec::with_capacity(args.len());
            for arg in args {
                arg_values.push(lookup(frame, *arg)?);
            }
            let callee_fn = resolver.resolve(callee.mangled()).unwrap_or_else(|| {
                panic!(
                    "interpreter: callee `{callee}` missing from IR — \
                     seal invariant violation",
                )
            });
            let result = execute_function(callee_fn, arg_values, resolver)?;
            frame.insert(*dest, result);
            Ok(())
        }
        IRInstruction::Const { dest, value } => {
            frame.insert(*dest, materialize_const(value));
            Ok(())
        }
        IRInstruction::UnaryOp { dest, op, operand } => {
            let operand_value = lookup(frame, *operand)?;
            let result = apply_unary_op(*op, operand_value)?;
            frame.insert(*dest, result);
            Ok(())
        }
    }
}

fn lookup(frame: &BTreeMap<ValueId, Value>, id: ValueId) -> Result<Value, RuntimeError> {
    frame
        .get(&id)
        .cloned()
        .ok_or(RuntimeError::ValueUndefined { id })
}

/// Materialize a `ConstValue` as a runtime [`Value`]. `Value::Int`
/// is a single `i64` slot; `UInt64` is reinterpreted as `i64` (the
/// seal pass forbids it from flowing through today, but the arm
/// stays exhaustive).
fn materialize_const(value: &ConstValue) -> Value {
    match value {
        ConstValue::Bool(b) => Value::Bool(*b),
        ConstValue::Int8(v) => Value::Int(*v as i64),
        ConstValue::Int16(v) => Value::Int(*v as i64),
        ConstValue::Int32(v) => Value::Int(*v as i64),
        ConstValue::Int64(v) => Value::Int(*v),
        ConstValue::String(s) => Value::String(s.clone()),
        ConstValue::UInt8(v) => Value::Int(*v as i64),
        ConstValue::UInt16(v) => Value::Int(*v as i64),
        ConstValue::UInt32(v) => Value::Int(*v as i64),
        ConstValue::UInt64(v) => Value::Int(*v as i64),
        ConstValue::Unit => Value::Unit,
    }
}
