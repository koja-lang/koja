//! Runtime values produced by the [`crate::Interp`] backend.
//!
//! Values mirror Expo's surface types (primitives, structs, enums,
//! lists, closures) and are reference-counted where Expo's
//! "shared on read, owned on move" semantics call for it. The
//! interpreter never aliases mutable state across processes -- list
//! mutation goes through `RefCell`, but the interpreter is
//! single-threaded today (process spawning is deferred).

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use expo_ir::MonomorphizedTypeIdentifier;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Binary(Rc<Vec<u8>>),
    Bool(bool),
    Closure(Rc<ClosureValue>),
    Enum(Rc<EnumValue>),
    Float(f64),
    Float32(f32),
    Int(i64),
    Int8(i8),
    Int16(i16),
    Int32(i32),
    List(Rc<RefCell<Vec<Value>>>),
    String(Rc<String>),
    Struct(Rc<StructValue>),
    UInt8(u8),
    UInt16(u16),
    UInt32(u32),
    UInt64(u64),
    Unit,
}

#[derive(Debug, PartialEq)]
pub struct StructValue {
    pub mangled: MonomorphizedTypeIdentifier,
    pub fields: Vec<(String, Value)>,
}

#[derive(Debug, PartialEq)]
pub struct EnumValue {
    pub mangled: MonomorphizedTypeIdentifier,
    pub variant: String,
    pub tag: u8,
    pub payload: VariantPayload,
}

#[derive(Debug, PartialEq)]
pub enum VariantPayload {
    Struct(Vec<(String, Value)>),
    Tuple(Vec<Value>),
    Unit,
}

#[derive(Debug, PartialEq)]
pub struct ClosureValue {
    pub callee: expo_ir::FunctionIdentifier,
    pub captures: Vec<(String, Value)>,
}

impl Value {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Float32(f) => Some(*f as f64),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Int8(i) => Some(*i as i64),
            Value::Int16(i) => Some(*i as i64),
            Value::Int32(i) => Some(*i as i64),
            Value::UInt8(u) => Some(*u as i64),
            Value::UInt16(u) => Some(*u as i64),
            Value::UInt32(u) => Some(*u as i64),
            Value::UInt64(u) => Some(*u as i64),
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
            Value::Binary(bytes) => write!(f, "<{}-byte binary>", bytes.len()),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Closure(c) => write!(f, "<closure {}>", c.callee),
            Value::Enum(e) => format_enum(f, e),
            Value::Float(x) => write!(f, "{x}"),
            Value::Float32(x) => write!(f, "{x}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Int8(i) => write!(f, "{i}"),
            Value::Int16(i) => write!(f, "{i}"),
            Value::Int32(i) => write!(f, "{i}"),
            Value::List(items) => format_list(f, &items.borrow()),
            Value::String(s) => write!(f, "{s:?}"),
            Value::Struct(s) => format_struct(f, s),
            Value::UInt8(u) => write!(f, "{u}"),
            Value::UInt16(u) => write!(f, "{u}"),
            Value::UInt32(u) => write!(f, "{u}"),
            Value::UInt64(u) => write!(f, "{u}"),
            Value::Unit => write!(f, "()"),
        }
    }
}

fn format_enum(f: &mut fmt::Formatter<'_>, e: &EnumValue) -> fmt::Result {
    match &e.payload {
        VariantPayload::Unit => write!(f, "{}", e.variant),
        VariantPayload::Tuple(values) => {
            write!(f, "{}(", e.variant)?;
            for (i, v) in values.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{v}")?;
            }
            write!(f, ")")
        }
        VariantPayload::Struct(fields) => {
            write!(f, "{}{{", e.variant)?;
            for (i, (name, v)) in fields.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{name}: {v}")?;
            }
            write!(f, "}}")
        }
    }
}

fn format_struct(f: &mut fmt::Formatter<'_>, s: &StructValue) -> fmt::Result {
    write!(f, "{}{{", s.mangled)?;
    for (i, (name, v)) in s.fields.iter().enumerate() {
        if i > 0 {
            write!(f, ", ")?;
        }
        write!(f, "{name}: {v}")?;
    }
    write!(f, "}}")
}

fn format_list(f: &mut fmt::Formatter<'_>, items: &[Value]) -> fmt::Result {
    write!(f, "[")?;
    for (i, v) in items.iter().enumerate() {
        if i > 0 {
            write!(f, ", ")?;
        }
        write!(f, "{v}")?;
    }
    write!(f, "]")
}
