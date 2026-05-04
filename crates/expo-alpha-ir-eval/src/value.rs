//! Runtime values produced by the [`crate::Interpreter`] backend.
//!
//! POC scope: just the variants that map 1:1 onto
//! [`expo_alpha_ir::ConstValue`]. New variants land as the IR vocabulary
//! grows (lists, strings, structs, enums, closures, …).

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Unit,
}

impl Value {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Unit => write!(f, "()"),
        }
    }
}
