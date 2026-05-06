//! Runtime values produced by the [`crate::Interpreter`] — variants
//! map 1:1 onto [`expo_alpha_ir::ConstValue`] for primitives, plus a
//! [`Value::Struct`] variant carrying the receiver's
//! [`expo_alpha_ir::IRSymbol`] and a positional `fields` vector
//! (indexed by [`expo_alpha_ir::IRStructField::index`]). New variants
//! (lists, enums, closures, …) land as the IR vocabulary grows.

use std::fmt;

use expo_alpha_ir::IRSymbol;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Float32(f32),
    Float64(f64),
    Int(i64),
    String(String),
    Struct {
        symbol: IRSymbol,
        fields: Vec<Value>,
    },
    Unit,
}

impl Value {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_float64(&self) -> Option<f64> {
        match self {
            Value::Float64(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_string(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Bool(b) => write!(f, "{b}"),
            // `{:?}` keeps `1.0` legible (vs `{}`'s `1`) so floats
            // round-trip through the auto-print contract and tests
            // can compare exact stdout.
            Value::Float32(v) => write!(f, "{v:?}"),
            Value::Float64(v) => write!(f, "{v:?}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::String(s) => f.write_str(s),
            Value::Struct { symbol, fields } => {
                write!(f, "{symbol}(")?;
                for (index, field) in fields.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{field}")?;
                }
                write!(f, ")")
            }
            Value::Unit => write!(f, "()"),
        }
    }
}
