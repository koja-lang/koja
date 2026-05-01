//! Per-call interpreter frame: SSA value table + named bindings.

use std::collections::HashMap;
use std::rc::Rc;

use expo_ir::{IROperand, IRValueId};
use expo_typecheck::types::Type;

use crate::error::RuntimeError;
use crate::value::Value;

#[derive(Debug, Default)]
pub struct Frame {
    /// Named local bindings populated by `StoreLocal` / parameter binding.
    pub locals: HashMap<String, Value>,
    /// SSA results indexed by `IRValueId`.
    pub values: HashMap<IRValueId, Value>,
    /// Stack of `PushTypeSubst` / `PopTypeSubst` entries. Tracked
    /// for symmetry with codegen even though the interpreter doesn't
    /// currently consult substitutions during dispatch (monomorphized
    /// IR carries concrete types in instruction operands).
    pub type_subst: Vec<HashMap<String, Type>>,
}

impl Frame {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve an [`IROperand`] to a runtime [`Value`]. Constant
    /// operands materialize directly; `Local(id)` looks up the SSA
    /// table and returns [`RuntimeError::UndefinedValue`] when the id
    /// hasn't been bound yet (which would indicate a lowering bug).
    pub fn materialize(&self, operand: &IROperand) -> Result<Value, RuntimeError> {
        match operand {
            IROperand::ConstBool(b) => Ok(Value::Bool(*b)),
            IROperand::ConstFloat(x) => Ok(Value::Float(*x)),
            IROperand::ConstInt(i) => Ok(Value::Int(*i)),
            IROperand::ConstStr(s) => Ok(Value::String(Rc::new(s.clone()))),
            IROperand::Local(id) => self
                .values
                .get(id)
                .cloned()
                .ok_or(RuntimeError::UndefinedValue(*id)),
            IROperand::Unit => Ok(Value::Unit),
        }
    }
}
