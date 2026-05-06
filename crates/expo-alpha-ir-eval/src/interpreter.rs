//! Tree-walking interpreter over a sealed [`IRProgram`] / [`IRScript`].
//! Parameterized over a [`CallResolver`] so both IR shapes share the
//! per-instruction execution, frame management, and terminator
//! dispatch code; only callee lookup differs. Operator math lives in
//! [`crate::ops`].

use std::collections::BTreeMap;

use expo_alpha_ir::{
    ConstValue, FunctionKind, IRBasicBlock, IRBlockId, IRFunction, IRInstruction, IRLocalId,
    IRProgram, IRScript, IRTerminator, ValueId,
};

use crate::error::RuntimeError;
use crate::intrinsics;
use crate::ops::{apply_binary_op, apply_unary_op};
use crate::value::Value;

pub struct Interpreter;

impl Interpreter {
    /// Execute the project-mode entry function and return its result.
    pub fn run_program(program: IRProgram) -> Result<Value, RuntimeError> {
        let entry = program.entry_function();
        execute_function(entry, Vec::new(), &program)
    }

    /// Execute the script-mode implicit body and return its trailing
    /// value.
    pub fn run_script(script: IRScript) -> Result<Value, RuntimeError> {
        let mut frame = Frame::new();
        execute_blocks(&script.blocks, &mut frame, &script)
    }
}

/// Per-call execution frame. SSA values and local-slot storage live
/// in separate maps so slot identity never collides with SSA
/// identity even though both keys happen to be `u32`.
struct Frame {
    values: BTreeMap<ValueId, Value>,
    locals: BTreeMap<IRLocalId, Value>,
}

impl Frame {
    fn new() -> Self {
        Self {
            values: BTreeMap::new(),
            locals: BTreeMap::new(),
        }
    }
}

/// Dereferences a `Call` callee by mangled symbol. Implemented by
/// both [`IRProgram`] and [`IRScript`] so one walker drives either.
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
/// its param `ValueId`s. Param promotion (entry-block `LocalDecl` +
/// `LocalWrite`) means the body reads from the slot, not the raw
/// param id; seeding `frame.values` keeps the promotion's
/// `LocalWrite { value: param.id }` resolvable. `@intrinsic`-tagged
/// functions route to [`crate::intrinsics`].
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
    if function.kind == FunctionKind::Intrinsic {
        return intrinsics::dispatch(function.symbol.mangled(), &args);
    }
    let mut frame = Frame::new();
    for (param, value) in function.params.iter().zip(args.into_iter()) {
        frame.values.insert(param.id, value);
    }

    execute_blocks(&function.blocks, &mut frame, resolver)
}

/// Drive a function body starting at `blocks[0]` until a `Return`
/// exits. The frame is shared across every block; unknown branch
/// targets panic per the seal contract.
fn execute_blocks<R: CallResolver>(
    blocks: &[IRBasicBlock],
    frame: &mut Frame,
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
                let cond_value = lookup(&frame.values, *cond)?;
                let Value::Bool(b) = cond_value else {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!("cond_branch expects a Bool condition; got {cond_value}",),
                    });
                };
                current = if b { *then_block } else { *else_block };
            }
            IRTerminator::Return { value: None } => return Ok(Value::Unit),
            IRTerminator::Return { value: Some(id) } => return lookup(&frame.values, *id),
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
    frame: &mut Frame,
    resolver: &R,
) -> Result<(), RuntimeError> {
    match instruction {
        IRInstruction::BinaryOp { dest, lhs, op, rhs } => {
            let lhs_value = lookup(&frame.values, *lhs)?;
            let rhs_value = lookup(&frame.values, *rhs)?;
            let result = apply_binary_op(*op, lhs_value, rhs_value)?;
            frame.values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::Call { dest, callee, args } => {
            let mut arg_values = Vec::with_capacity(args.len());
            for arg in args {
                arg_values.push(lookup(&frame.values, *arg)?);
            }
            let callee_fn = resolver.resolve(callee.mangled()).unwrap_or_else(|| {
                panic!(
                    "interpreter: callee `{callee}` missing from IR — \
                     seal invariant violation",
                )
            });
            let result = execute_function(callee_fn, arg_values, resolver)?;
            frame.values.insert(*dest, result);
            Ok(())
        }
        IRInstruction::Const { dest, value } => {
            frame.values.insert(*dest, materialize_const(value));
            Ok(())
        }
        IRInstruction::FieldGet {
            base,
            dest,
            field_index,
            field_type: _,
            struct_symbol: _,
        } => {
            let base_value = lookup(&frame.values, *base)?;
            let Value::Struct { fields, .. } = base_value else {
                return Err(RuntimeError::TypeMismatch {
                    detail: format!("field_get expects a Struct receiver; got {base_value}",),
                });
            };
            let field = fields
                .into_iter()
                .nth(*field_index as usize)
                .unwrap_or_else(|| {
                    panic!(
                        "interpreter: FieldGet index {field_index} out of range — \
                         seal invariant violation",
                    )
                });
            frame.values.insert(*dest, field);
            Ok(())
        }
        // Slot identity comes from `LocalWrite`; `LocalDecl` is a
        // no-op for the interpreter (the LLVM backend uses it to
        // emit an entry-block alloca).
        IRInstruction::LocalDecl { .. } => Ok(()),
        IRInstruction::LocalRead { dest, local, .. } => {
            let value = frame.locals.get(local).cloned().unwrap_or_else(|| {
                panic!(
                    "interpreter: `LocalRead` of `{local}` before its `LocalWrite` — \
                     seal invariant violation",
                )
            });
            frame.values.insert(*dest, value);
            Ok(())
        }
        IRInstruction::LocalWrite { local, value } => {
            let resolved = lookup(&frame.values, *value)?;
            frame.locals.insert(*local, resolved);
            Ok(())
        }
        IRInstruction::StructInit { dest, fields, ty } => {
            let mut materialized = Vec::with_capacity(fields.len());
            for field in fields {
                materialized.push(lookup(&frame.values, field.value)?);
            }
            frame.values.insert(
                *dest,
                Value::Struct {
                    symbol: ty.clone(),
                    fields: materialized,
                },
            );
            Ok(())
        }
        IRInstruction::UnaryOp { dest, op, operand } => {
            let operand_value = lookup(&frame.values, *operand)?;
            let result = apply_unary_op(*op, operand_value)?;
            frame.values.insert(*dest, result);
            Ok(())
        }
    }
}

fn lookup(values: &BTreeMap<ValueId, Value>, id: ValueId) -> Result<Value, RuntimeError> {
    values
        .get(&id)
        .cloned()
        .ok_or(RuntimeError::ValueUndefined { id })
}

/// Materialize a `ConstValue` as a runtime [`Value`]. Every int
/// width collapses to `Value::Int(i64)` (the seal pass keeps
/// width-mismatched flows out, but the arms stay exhaustive).
fn materialize_const(value: &ConstValue) -> Value {
    match value {
        ConstValue::Bool(b) => Value::Bool(*b),
        ConstValue::Float32(v) => Value::Float32(*v),
        ConstValue::Float64(v) => Value::Float64(*v),
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
