//! Runtime values produced by the [`crate::Interpreter`] — variants
//! map 1:1 onto [`expo_alpha_ir::ConstValue`] for primitives, plus a
//! [`Value::Struct`] variant carrying the receiver's
//! [`expo_alpha_ir::IRSymbol`] and a positional `fields` vector
//! (indexed by [`expo_alpha_ir::IRStructField::index`]) and a
//! [`Value::Enum`] variant carrying the receiver enum's
//! [`expo_alpha_ir::IRSymbol`], the discriminant
//! [`expo_alpha_ir::IRVariantTag`], the variant `name` (cached for
//! `Display`), and a per-shape [`EnumPayload`]. New variants (lists,
//! closures, …) land as the IR vocabulary grows.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use expo_alpha_ir::{IRSymbol, IRVariantTag};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Owned heap bytes; `bit_length` is implicitly `bytes.len() * 8`.
    Binary(Vec<u8>),
    /// Owned heap bits. `bit_length` may be a non-multiple of 8 —
    /// payload occupies `ceil(bit_length / 8)` bytes; trailing bits
    /// in the last byte are zero-padded.
    Bits {
        bytes: Vec<u8>,
        bit_length: u64,
    },
    Bool(bool),
    /// First-class closure value. `body` resolves through the
    /// interpreter's call resolver to a `FunctionKind::Closure`
    /// `IRFunction`; `captures` is the env array indexed by every
    /// `IRInstruction::LoadCapture` inside the body. Captureless
    /// closures (the fn-as-value adapter shape) carry an empty
    /// captures vec.
    Closure {
        body: IRSymbol,
        captures: Vec<Value>,
    },
    Enum {
        name: String,
        payload: EnumPayload,
        symbol: IRSymbol,
        tag: IRVariantTag,
    },
    Float32(f32),
    Float64(f64),
    Int(i64),
    /// Heap-backed dynamic array. Shared `Rc<RefCell>` so move-self
    /// intrinsics (`append`, `pop`, `concat`) can mutate the
    /// underlying buffer in place — the interpreter copies the `Rc`,
    /// not the `Vec`. Aliased reads observe the post-mutation state,
    /// matching the LLVM by-value ABI's conservative copy-on-write
    /// behavior in practice (every alpha intrinsic that mutates
    /// consumes its receiver via `move self`).
    List(Rc<RefCell<Vec<Value>>>),
    String(String),
    Struct {
        symbol: IRSymbol,
        fields: Vec<Value>,
    },
    Unit,
}

/// Materialized payload for a [`Value::Enum`]. Mirrors
/// [`expo_alpha_ir::IRVariantPayload`] one-to-one but carries
/// already-evaluated [`Value`]s. The `Struct` arm carries
/// `(field_name, value)` pairs in declaration order so `Display`
/// can render named fields without a registry handle.
#[derive(Debug, Clone, PartialEq)]
pub enum EnumPayload {
    Struct(Vec<(String, Value)>),
    Tuple(Vec<Value>),
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
            Value::Binary(bytes) => write_binary_bytes(f, bytes),
            Value::Bits { bytes, bit_length } => write_bits_bytes(f, bytes, *bit_length),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Closure { body, captures } => {
                write!(f, "<closure {body}")?;
                if !captures.is_empty() {
                    write!(f, " env=[")?;
                    for (index, capture) in captures.iter().enumerate() {
                        if index > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{capture}")?;
                    }
                    write!(f, "]")?;
                }
                write!(f, ">")
            }
            Value::Enum {
                symbol,
                name,
                payload,
                ..
            } => {
                write!(f, "{symbol}.{name}")?;
                match payload {
                    EnumPayload::Struct(fields) => {
                        write!(f, "{{")?;
                        for (index, (field_name, value)) in fields.iter().enumerate() {
                            if index > 0 {
                                write!(f, ", ")?;
                            }
                            write!(f, "{field_name}: {value}")?;
                        }
                        write!(f, "}}")
                    }
                    EnumPayload::Tuple(values) => {
                        write!(f, "(")?;
                        for (index, value) in values.iter().enumerate() {
                            if index > 0 {
                                write!(f, ", ")?;
                            }
                            write!(f, "{value}")?;
                        }
                        write!(f, ")")
                    }
                    EnumPayload::Unit => Ok(()),
                }
            }
            // `{:?}` keeps `1.0` legible (vs `{}`'s `1`) so floats
            // round-trip through the auto-print contract and tests
            // can compare exact stdout.
            Value::Float32(v) => write!(f, "{v:?}"),
            Value::Float64(v) => write!(f, "{v:?}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::List(items) => write_list_items(f, &items.borrow()),
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

/// Render a [`Value::List`] as `[a, b, c]`. Element values are
/// formatted with their own `Display` impl so nested lists / structs
/// round-trip cleanly.
fn write_list_items(f: &mut fmt::Formatter<'_>, items: &[Value]) -> fmt::Result {
    write!(f, "[")?;
    for (index, value) in items.iter().enumerate() {
        if index > 0 {
            write!(f, ", ")?;
        }
        write!(f, "{value}")?;
    }
    write!(f, "]")
}

/// Render a [`Value::Binary`] as `<<0x48, 0x65>>`. Mirrors the LLVM
/// runtime printer's output so eval / native produce byte-identical
/// stdout for tests.
fn write_binary_bytes(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    write!(f, "<<")?;
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            write!(f, ", ")?;
        }
        write!(f, "0x{byte:02X}")?;
    }
    write!(f, ">>")
}

/// Render a [`Value::Bits`] as `<<0x48, 0b101::3>>`. Byte-aligned
/// `Bits` (rare but legal) reuses the [`write_binary_bytes`] shape;
/// non-byte-aligned tails render the trailing partial byte as a
/// width-suffixed binary literal.
fn write_bits_bytes(f: &mut fmt::Formatter<'_>, bytes: &[u8], bit_length: u64) -> fmt::Result {
    let trailing_bits = (bit_length % 8) as u8;
    if trailing_bits == 0 {
        return write_binary_bytes(f, bytes);
    }
    let full_bytes = bytes.len().saturating_sub(1);
    write!(f, "<<")?;
    for (index, byte) in bytes.iter().take(full_bytes).enumerate() {
        if index > 0 {
            write!(f, ", ")?;
        }
        write!(f, "0x{byte:02X}")?;
    }
    if full_bytes > 0 {
        write!(f, ", ")?;
    }
    let tail = bytes.last().copied().unwrap_or(0) >> (8 - trailing_bits);
    write!(
        f,
        "0b{tail:0>width$b}::{trailing_bits}",
        width = trailing_bits as usize
    )?;
    write!(f, ">>")
}
