//! Tree-walking interpreter over a sealed [`IRProgram`].
//!
//! Construct with [`Interpreter::new`] (no validation work — the program
//! is already sealed by `expo-ir-v2::lower_program`), then call
//! [`Interpreter::run`] to execute the entry function and receive the
//! returned [`Value`].

use std::collections::BTreeMap;

use expo_ir_v2::{
    ConstValue, IRBasicBlock, IRBinOp, IRFunction, IRInstruction, IRProgram, IRTerminator, ValueId,
};

use crate::error::RuntimeError;
use crate::value::Value;

pub struct Interpreter {
    program: IRProgram,
}

impl Interpreter {
    pub fn new(program: IRProgram) -> Self {
        Self { program }
    }

    /// Execute the program's entry function and return the value it
    /// produces (or `Value::Unit` if the entry returns nothing).
    pub fn run(&self) -> Result<Value, RuntimeError> {
        let entry = self.program.entry_function();
        execute_function(entry)
    }
}

/// Run `function` to completion. POC scope guarantees a single basic
/// block per function, so this loops until the terminator says so —
/// branches land when control flow does.
fn execute_function(function: &IRFunction) -> Result<Value, RuntimeError> {
    let mut frame: BTreeMap<ValueId, Value> = BTreeMap::new();
    let block = function
        .blocks
        .first()
        .expect("sealed IRFunction has at least one basic block");
    execute_block(block, &mut frame)
}

fn execute_block(
    block: &IRBasicBlock,
    frame: &mut BTreeMap<ValueId, Value>,
) -> Result<Value, RuntimeError> {
    for instruction in &block.instructions {
        execute_instruction(instruction, frame)?;
    }
    follow_terminator(&block.terminator, frame)
}

fn execute_instruction(
    instruction: &IRInstruction,
    frame: &mut BTreeMap<ValueId, Value>,
) -> Result<(), RuntimeError> {
    match instruction {
        IRInstruction::BinaryOp { dest, lhs, op, rhs } => {
            let lhs_value = lookup(frame, *lhs)?;
            let rhs_value = lookup(frame, *rhs)?;
            let result = apply_binary_op(*op, lhs_value, rhs_value)?;
            frame.insert(*dest, result);
            Ok(())
        }
        IRInstruction::Const { dest, value } => {
            frame.insert(*dest, materialize_const(value));
            Ok(())
        }
    }
}

fn follow_terminator(
    terminator: &IRTerminator,
    frame: &BTreeMap<ValueId, Value>,
) -> Result<Value, RuntimeError> {
    match terminator {
        IRTerminator::Return { value: None } => Ok(Value::Unit),
        IRTerminator::Return { value: Some(id) } => lookup(frame, *id),
    }
}

fn lookup(frame: &BTreeMap<ValueId, Value>, id: ValueId) -> Result<Value, RuntimeError> {
    frame
        .get(&id)
        .cloned()
        .ok_or(RuntimeError::ValueUndefined { id })
}

fn materialize_const(value: &ConstValue) -> Value {
    match value {
        ConstValue::Bool(b) => Value::Bool(*b),
        ConstValue::Int(i) => Value::Int(*i),
        ConstValue::Unit => Value::Unit,
    }
}

fn apply_binary_op(op: IRBinOp, lhs: Value, rhs: Value) -> Result<Value, RuntimeError> {
    let (Value::Int(a), Value::Int(b)) = (&lhs, &rhs) else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("{op:?} expects two Int operands; got {lhs} and {rhs}"),
        });
    };
    let (a, b) = (*a, *b);
    let checked = match op {
        IRBinOp::Add => a.checked_add(b),
        IRBinOp::Div => {
            if b == 0 {
                return Err(RuntimeError::DivisionByZero { op });
            }
            a.checked_div(b)
        }
        IRBinOp::Mod => {
            if b == 0 {
                return Err(RuntimeError::DivisionByZero { op });
            }
            a.checked_rem(b)
        }
        IRBinOp::Mul => a.checked_mul(b),
        IRBinOp::Sub => a.checked_sub(b),
    };
    checked
        .map(Value::Int)
        .ok_or(RuntimeError::IntegerOverflow { lhs: a, op, rhs: b })
}
