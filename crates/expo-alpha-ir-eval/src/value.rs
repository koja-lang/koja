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

/// `Map<K, V>` storage: `(key, value)` pairs in insertion order.
/// Eval doesn't need a real hash table — linear probes over a Vec
/// give the right semantics in tests' tiny working sets, and `Map`
/// values are `Rc<RefCell<...>>` for the same in-place-mutation
/// reasons as [`Value::List`] (every collection-mutating intrinsic
/// is `move self`).
pub type MapEntries = Rc<RefCell<Vec<(Value, Value)>>>;

/// `Set<T>` storage: unique elements in insertion order. Same
/// motivation as [`MapEntries`] for the `Rc<RefCell<...>>` shape.
pub type SetEntries = Rc<RefCell<Vec<Value>>>;

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
    /// Raw C pointer — backs `CPtr<T>` and the `@extern "C"` shims
    /// in [`crate::externs`] that traffic in pointers. Eval is
    /// single-threaded and in-process, so the pointer is valid for
    /// the duration of its referent — same memory the LLVM backend
    /// would observe. The element type `T` is type-level only;
    /// intrinsic emitters consult `function.params[0].ty` /
    /// `function.return_type` when they need its size.
    CPtr(*mut u8),
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
    /// Heap-backed associative map keyed by [`Value`]. Eval uses
    /// linear probes for `Eq` (matches Expo's `Equality` protocol's
    /// `eq` shape — every key compared by value, no hashing). Same
    /// `Rc<RefCell<…>>` motivation as [`Value::List`] for in-place
    /// mutation.
    Map(MapEntries),
    /// Heap-backed unique-element set. Same shape rationale as
    /// [`Value::Map`].
    Set(SetEntries),
    /// Byte payload backing an Expo `String`. The runtime ABI
    /// doesn't enforce UTF-8 (every Expo string is "bytes that
    /// happen to render as UTF-8 most of the time"), so eval stores
    /// raw bytes here too — matches v1's permissive treatment.
    /// Chains like `Random.bytes(n).to_string().to_binary()` rely
    /// on flowing arbitrary bytes through a `String` value without
    /// the interpreter rejecting non-UTF-8 payloads.
    String(Vec<u8>),
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

    /// Borrow the bytes backing a [`Value::String`]. Use this when
    /// the operation is byte-oriented (concat, byte length, FFI
    /// passthrough) — it sidesteps the UTF-8 validity question
    /// entirely.
    pub fn as_string_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::String(bytes) => Some(bytes.as_slice()),
            _ => None,
        }
    }

    /// Borrow a [`Value::String`] as `&str` when its bytes are
    /// valid UTF-8. Returns `None` for non-string values or when
    /// the payload isn't valid UTF-8 — callers that need codepoint
    /// semantics surface a clean error in the latter case.
    pub fn as_string(&self) -> Option<&str> {
        match self {
            Value::String(bytes) => std::str::from_utf8(bytes).ok(),
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
            Value::CPtr(ptr) => write_cptr(f, *ptr),
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
            Value::Map(entries) => write_map_entries(f, &entries.borrow()),
            Value::Set(items) => write_set_items(f, &items.borrow()),
            Value::String(bytes) => f.write_str(&String::from_utf8_lossy(bytes)),
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

/// Render a [`Value::CPtr`] as `<cptr null>` or `<cptr 0x...>`.
/// Mirrors the LLVM runtime printer's shape so eval / native produce
/// byte-identical stdout when a pointer leaks into `print`.
fn write_cptr(f: &mut fmt::Formatter<'_>, ptr: *mut u8) -> fmt::Result {
    if ptr.is_null() {
        write!(f, "<cptr null>")
    } else {
        write!(f, "<cptr 0x{:x}>", ptr as usize)
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

/// Render a [`Value::Map`] as `[k1: v1, k2: v2]`. Empty maps render
/// as `[:]` to disambiguate from an empty list literal — matches
/// the source-level convention.
fn write_map_entries(f: &mut fmt::Formatter<'_>, entries: &[(Value, Value)]) -> fmt::Result {
    if entries.is_empty() {
        return write!(f, "[:]");
    }
    write!(f, "[")?;
    for (index, (key, value)) in entries.iter().enumerate() {
        if index > 0 {
            write!(f, ", ")?;
        }
        write!(f, "{key}: {value}")?;
    }
    write!(f, "]")
}

/// Render a [`Value::Set`] as `{a, b, c}`. Empty sets render as
/// `{}`. Curly braces (vs the list literal's brackets) make the
/// shape unambiguous in eval's debug output even though the source
/// syntax for set literals reuses `[...]`.
fn write_set_items(f: &mut fmt::Formatter<'_>, items: &[Value]) -> fmt::Result {
    write!(f, "{{")?;
    for (index, value) in items.iter().enumerate() {
        if index > 0 {
            write!(f, ", ")?;
        }
        write!(f, "{value}")?;
    }
    write!(f, "}}")
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
