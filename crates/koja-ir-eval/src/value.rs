//! Runtime values produced by the [`crate::Interpreter`]. Variants
//! map 1:1 onto [`koja_ir::ConstValue`] for primitives, plus
//! composite variants ([`Value::Struct`], [`Value::Enum`], lists,
//! closures, ...) that carry their receiver's [`koja_ir::IRSymbol`]
//! and already-evaluated payloads. New variants land as the IR
//! vocabulary grows.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;
use std::str;

use koja_ir::{IRSymbol, IRVariantTag};

/// `Map<K, V>` storage, `(key, value)` pairs in insertion order.
/// Linear probes over a `Vec` give the right semantics without a
/// hash table.
pub type MapEntries = Rc<RefCell<Vec<(Value, Value)>>>;

/// `Set<T>` storage, unique elements in insertion order.
pub type SetEntries = Rc<RefCell<Vec<Value>>>;

/// A runtime value. Heap-backed variants share their storage through
/// `Rc`, so `Value::clone` is a refcount bump. Value semantics hold
/// because a mutator either builds a fresh value or, when the storage
/// is uniquely held, mutates it in place (see
/// `intrinsics::consuming`). No mutation is observable through
/// another binding.
///
/// The derived `PartialEq` is structural and is what `Map` key and
/// `Set` element lookups use, so two values with equal contents
/// match regardless of which `Rc` holds them.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Heap bytes with `bit_length` implicitly `bytes.len() * 8`.
    Binary(Rc<Vec<u8>>),
    /// Heap bits. `bit_length` may be a non-multiple of 8. The
    /// payload occupies `ceil(bit_length / 8)` bytes, with trailing
    /// bits in the last byte zero-padded.
    Bits {
        bytes: Rc<Vec<u8>>,
        bit_length: u64,
    },
    Bool(bool),
    /// Raw C pointer backing `CPtr<T>` and the `@extern "C"` shims
    /// in [`crate::externs`] that traffic in pointers. Eval is
    /// single-threaded and in-process, so the pointer is valid for
    /// the duration of its referent. The element type `T` is
    /// type-level only. Intrinsic emitters consult
    /// `function.params[0].ty` / `function.return_type` when they
    /// need its size.
    CPtr(*mut u8),
    /// First-class closure value. `body` resolves through the
    /// interpreter's call resolver to a `FunctionKind::Closure`
    /// `IRFunction`. `captures` is the env array indexed by every
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
    /// Every integer width, signed or unsigned, stored in one `i64`.
    /// Unsigned 64-bit values keep their bit pattern, so `u64::MAX`
    /// is stored as `-1`. Sites that need the width read it from the
    /// [`koja_ir::IRType`] in hand.
    Int(i64),
    /// Heap-backed dynamic array.
    List(Rc<RefCell<Vec<Value>>>),
    /// Heap-backed map keyed by [`Value`], compared by `==` like
    /// Koja's `Equality` protocol.
    Map(MapEntries),
    /// Heap-backed unique-element set.
    Set(SetEntries),
    /// Valid UTF-8 bytes backing a Koja `String`.
    String(Rc<Vec<u8>>),
    Struct {
        symbol: IRSymbol,
        fields: Vec<Value>,
    },
    /// Anonymous tuple. Structural (no symbol), mirroring
    /// [`koja_ir::IRType::Tuple`].
    Tuple(Vec<Value>),
    /// Tagged union value. `tag` is the 0-based member index (the
    /// position of the wrapped member in the union's canonical
    /// member list). `payload` is the boxed value the user wrote.
    /// `symbol` is the union's mangled name, kept for debug
    /// rendering and for sanity-checking against the IR's
    /// `IRType::Union { mangled }`.
    Union {
        payload: Box<Value>,
        symbol: IRSymbol,
        tag: u8,
    },
    Unit,
}

/// Materialized payload for a [`Value::Enum`], mirroring
/// [`koja_ir::IRVariantPayload`]. The `Struct` arm carries
/// `(field_name, value)` pairs in declaration order so `Display`
/// can render named fields without a registry handle. Payloads are
/// immutable, so plain `Rc` sharing suffices.
#[derive(Debug, Clone, PartialEq)]
pub enum EnumPayload {
    Struct(Rc<Vec<(String, Value)>>),
    Tuple(Rc<Vec<Value>>),
    Unit,
}

impl EnumPayload {
    /// Wrap owned struct-payload fields.
    pub fn struct_fields(fields: Vec<(String, Value)>) -> EnumPayload {
        EnumPayload::Struct(Rc::new(fields))
    }

    /// Wrap owned tuple-payload values.
    pub fn tuple(values: Vec<Value>) -> EnumPayload {
        EnumPayload::Tuple(Rc::new(values))
    }
}

impl Value {
    /// Construct a [`Value::Binary`] from owned bytes.
    pub fn binary(bytes: impl Into<Vec<u8>>) -> Value {
        Value::Binary(Rc::new(bytes.into()))
    }

    /// Construct a [`Value::Bits`] from owned bytes and a bit length.
    pub fn bits(bytes: impl Into<Vec<u8>>, bit_length: u64) -> Value {
        Value::Bits {
            bytes: Rc::new(bytes.into()),
            bit_length,
        }
    }

    /// Construct a [`Value::String`] from owned bytes or anything
    /// convertible (`String`, `Vec<u8>`, `&[u8]`, `&str`).
    pub fn string(bytes: impl Into<Vec<u8>>) -> Value {
        let bytes = bytes.into();
        assert!(
            str::from_utf8(&bytes).is_ok(),
            "Koja String payload must be valid UTF-8",
        );
        Value::String(Rc::new(bytes))
    }

    /// Read a [`Value::Bool`], or `None` for any other variant.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Read a [`Value::Float64`], or `None` for any other variant.
    pub fn as_float64(&self) -> Option<f64> {
        match self {
            Value::Float64(v) => Some(*v),
            _ => None,
        }
    }

    /// Read a [`Value::Int`], or `None` for any other variant.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Borrow the bytes backing a [`Value::String`]. Use this when
    /// the operation is byte-oriented (concat, byte length, FFI
    /// passthrough), since it sidesteps the UTF-8 validity question
    /// entirely.
    pub fn as_string_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::String(bytes) => Some(bytes.as_slice()),
            _ => None,
        }
    }

    /// Whether both values are views of the same reference-counted
    /// storage, for the kinds the consuming sites mutate in place.
    pub fn shares_storage(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Binary(a), Value::Binary(b)) | (Value::String(a), Value::String(b)) => {
                Rc::ptr_eq(a, b)
            }
            (Value::List(a), Value::List(b)) | (Value::Set(a), Value::Set(b)) => Rc::ptr_eq(a, b),
            (Value::Map(a), Value::Map(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Borrow a [`Value::String`] as `&str` when its bytes are
    /// valid UTF-8. Returns `None` for non-string values or when
    /// the payload isn't valid UTF-8, where callers that need
    /// codepoint semantics surface a clean error.
    pub fn as_string(&self) -> Option<&str> {
        match self {
            Value::String(bytes) => str::from_utf8(bytes).ok(),
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
            // `{:?}` keeps `1.0` legible (vs `{}`'s `1`) so float
            // rendering matches `Debug.format` and tests can compare
            // exact stdout.
            Value::Float32(v) => write!(f, "{v:?}"),
            Value::Float64(v) => write!(f, "{v:?}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::List(items) => write_list_items(f, &items.borrow()),
            Value::Map(entries) => write_map_entries(f, &entries.borrow()),
            Value::Set(items) => write_set_items(f, &items.borrow()),
            Value::String(bytes) => f.write_str(&String::from_utf8_lossy(bytes)),
            Value::Tuple(elements) => {
                write!(f, "(")?;
                for (index, element) in elements.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{element}")?;
                }
                write!(f, ")")
            }
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
            Value::Union {
                payload,
                symbol,
                tag,
            } => write!(f, "{symbol}#{tag}({payload})"),
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
/// as `[:]` to disambiguate from an empty list literal, matching
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
/// `Bits` (rare but legal) reuses the [`write_binary_bytes`] shape.
/// Non-byte-aligned tails render the trailing partial byte as a
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
