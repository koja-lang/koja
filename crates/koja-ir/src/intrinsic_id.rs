//! Typed dispatch id for `@intrinsic`-annotated functions. Replaces
//! the prior free-form `id: String` (joined from the function's
//! identifier path) with an exhaustive enum so both backends
//! ([`koja_ir_llvm`] and [`koja_ir_eval`]) match a
//! finite, compiler-checked universe instead of re-parsing strings
//! through ad-hoc `matches_id` / `method_for` / `op_from_id`
//! helpers.
//!
//! [`IRIntrinsicId::from_identifier`] is the only producer: lift
//! consumes a function's [`Identifier`], strips the package prefix,
//! and walks the remaining path segments. An unknown segmentation
//! returns `None` so the caller can surface a clean diagnostic
//! (typo'd `@intrinsic` decl) instead of panicking at codegen.
//!
//! [`Display`] mirrors the historical `id` strings (`"Kernel.panic"`,
//! `"CPtr.null?"`, `"Int8.band"`) so existing diagnostics and test
//! fixtures keep their wording. Backends never go through `Display`
//! for dispatch — they pattern-match the enum directly.

use std::fmt;

use koja_ast::identifier::Identifier;

/// One `@intrinsic`-annotated function's dispatch slot. Constructed
/// at lift via [`IRIntrinsicId::from_identifier`]; consumed by both
/// backend dispatch tables via exhaustive `match`.
///
/// Single-method namespaces (`Kernel`, `CString`, `Bits`) still wrap
/// their inner method enum even though it has one variant today,
/// so adding a sibling method later is a variant-add rather than a
/// shape change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IRIntrinsicId {
    Binary(BinaryMethod),
    Bits(BitsMethod),
    Bitwise {
        ty: IntType,
        op: BitOp,
    },
    CPtr(CPtrMethod),
    CString(CStringMethod),
    Debug(DebugImpl),
    Equality(EqualityImpl),
    Hash(HashImpl),
    Kernel(KernelMethod),
    List(ListMethod),
    Map(MapMethod),
    /// Explicit conversions out of the hub types — the inverse of
    /// implicit hub widening. `Int.to_<width>` and `UInt64.to_int`
    /// are checked (`Result<T, NumericConversionError>`); `Float.to_float32`
    /// is total (rounds to nearest).
    NumericConvert(NumericConvert),
    Parse(ParseTarget),
    /// Transitional. The script-mode test fixture's `@intrinsic fn
    /// print(s: String)`. Goes away when the `Debug` protocol
    /// displaces it.
    Print,
    /// `@intrinsic` statics on the `Process` protocol from
    /// [`koja/lib/global/src/process.koja`], dispatched on the
    /// protocol name with no receiver value.
    Process(ProcessMethod),
    /// `@intrinsic` methods on `Ref<M, R>` from
    /// [`koja/lib/global/src/process.koja`]. The `M` / `R` type
    /// parameters don't appear here — they ride the
    /// [`crate::IRFunction`] signature, the same way `List<T>`'s
    /// element type does.
    Ref(RefMethod),
    /// `@intrinsic` method on `ReplyTo<R>`. Single-method namespace
    /// today (`send`); the wrapper enum keeps adding a sibling
    /// method later a variant-add rather than a shape change, like
    /// [`KernelMethod`] / [`BitsMethod`].
    ReplyTo(ReplyToMethod),
    Set(SetMethod),
    /// `@intrinsic` methods on `Socket` from
    /// [`koja/lib/net/src/net.koja`]. Both methods bridge into the
    /// runtime's `koja_socket_*` C ABI (`recv_from` -> mailbox-driven
    /// recv with sender address, `resolve` -> blocking
    /// `getaddrinfo`). Wrapped in an enum so adding sibling methods
    /// (e.g. `send_to_async`) is a variant-add rather than a shape
    /// change.
    Socket(SocketMethod),
    String(StringMethod),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelMethod {
    Panic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CPtrMethod {
    Alloc,
    Free,
    Null,
    NullQ,
    Offset,
    Read,
    ToBinary,
    ToString,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CStringMethod {
    ToString,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryMethod {
    At,
    ByteSize,
    Ptr,
    Slice,
    ToBits,
    ToString,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitsMethod {
    ToBinary,
}

/// Methods on `List<T>`. The element type doesn't appear here because
/// the IR carries it on the [`crate::IRFunction`] signature; backends
/// monomorphize per element type from there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListMethod {
    Append,
    Concat,
    EmptyQ,
    FromList,
    Get,
    Length,
    New,
    Pop,
    ReplaceAt,
    Slice,
}

/// Methods on `Map<K, V>`. Like [`ListMethod`], the key + value
/// types don't appear here — both ride the [`crate::IRFunction`]
/// signature, and backends specialize layouts per `(K, V)` pair
/// from there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapMethod {
    EmptyQ,
    FromMap,
    Get,
    HasQ,
    Length,
    New,
    Put,
    Remove,
}

/// Methods on `Set<T>`. Same monomorphization story as
/// [`ListMethod`] / [`MapMethod`] — the element type rides the
/// [`crate::IRFunction`] signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetMethod {
    EmptyQ,
    FromList,
    HasQ,
    Insert,
    Length,
    New,
    Remove,
}

/// `@intrinsic`-flagged methods on `Ref<M, R>` from
/// [`koja/lib/global/src/process.koja`]. `Cast` / `Call` / `Signal` /
/// `Kill` / `AliveQ` / `SendAfter` cover the public mailbox surface;
/// `SelfRef` is the only zero-argument constructor (the others are
/// receiver-bound).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefMethod {
    AliveQ,
    Call,
    Cast,
    Kill,
    SelfRef,
    SendAfter,
    Signal,
}

/// `@intrinsic`-flagged statics on the `Process` protocol: `Monitor`
/// registers the calling process as a watcher of a `Pid`, `Demonitor`
/// retracts one, `Parent` reports the calling process's spawner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessMethod {
    Demonitor,
    Monitor,
    Parent,
}

/// `@intrinsic`-flagged method on `ReplyTo<R>`. Single-variant today
/// (`send`); kept as a wrapper enum so adding a sibling later is a
/// variant-add, not a shape change. Mirrors [`BitsMethod`] /
/// [`KernelMethod`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyToMethod {
    Send,
}

/// `@intrinsic`-flagged methods on `Socket` from
/// [`koja/lib/net/src/net.koja`]. `RecvFrom` receives one
/// datagram + sender address (suspending the process until the fd
/// is readable); `Resolve` is a synchronous `getaddrinfo` shim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketMethod {
    RecvFrom,
    Resolve,
}

/// Methods on `String` flagged `@intrinsic` in
/// [`crate::stdlib::string`]. Excludes `eq` / `hash`; those route
/// through [`EqualityImpl::String`] / [`HashImpl::String`] alongside
/// the other primitive impls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringMethod {
    ByteLength,
    Get,
    Length,
    Slice,
    ToBinary,
    ToCstring,
}

/// Receiver shape for `Debug.format` impls. Mirrors
/// [`EqualityImpl`] / [`HashImpl`]'s flat-enum-with-`Int(IntType)`
/// pattern: `Bool` and `Float` / `Float32` are siblings rather than
/// folded into a catch-all "primitive" enum because each variant
/// has its own emitter cell (boolean rendering, integer
/// `format("{}")`, IEEE-754 `format("{}")` with f32/f64 width).
///
/// `String` isn't here — `String.format` ships a pure-Koja body
/// (`"\"" <> self.escape_debug() <> "\""` in
/// `lib/global/src/debug.koja`) instead of an intrinsic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugImpl {
    Bool,
    Float,
    Float32,
    Int(IntType),
}

/// Receiver shape for `Equality.eq` impls. One variant per emitter
/// shape: `icmp` for `Bool` + integers, `fcmp` for floats, and a
/// length-aware runtime helper for `String`. Numeric widths are folded
/// into nested [`IntType`] / [`FloatType`] enums (each width its own emitter
/// cell) so the outer arms stay one-to-one with the emitter family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EqualityImpl {
    Bool,
    Float(FloatType),
    Int(IntType),
    String,
}

/// Mirrors [`EqualityImpl`] for the `Hash.hash` family. Each variant
/// keeps its own emitter cell (SplitMix64 on the boolean extension,
/// SplitMix64 on the value bits, FNV-style on string bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashImpl {
    Bool,
    Int(IntType),
    String,
}

/// Integer receivers for the 48-cell `Bitwise` family and the
/// 8-cell integer slice of `Equality` / `Hash`. `Bool` and `String`
/// are not included — they're siblings of [`EqualityImpl::Int`] /
/// [`HashImpl::Int`] at the enum level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntType {
    Int,
    Int8,
    Int16,
    Int32,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
}

/// Float receivers for the float slice of `Equality`. Sibling of
/// [`IntType`]; each width is its own emitter cell (LLVM picks
/// f32 / f64 compare from the param's actual type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatType {
    Float,
    Float32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitOp {
    Band,
    Bnot,
    Bor,
    Bsl,
    Bsr,
    Bxor,
}

/// One explicit numeric conversion method. `IntNarrow` covers the
/// seven `Int.to_*` methods (target width carried here, not read
/// back out of the return type); `UInt64ToInt` is the one sized
/// type with no implicit path to the hub; `FloatToFloat32` is the
/// float counterpart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericConvert {
    FloatToFloat32,
    IntNarrow(IntNarrowTarget),
    UInt64ToInt,
}

/// Target width for a checked `Int.to_<width>` narrowing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntNarrowTarget {
    Int8,
    Int16,
    Int32,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseTarget {
    Float,
    Int,
}

impl IRIntrinsicId {
    /// Map a function's canonical identifier to its dispatch slot.
    /// Returns `None` if no registered backend handles the
    /// `(receiver, method)` pair — lift surfaces a diagnostic so
    /// typo'd `@intrinsic` decls fail at parse -> check time, not
    /// at codegen.
    ///
    /// Strips the package prefix and walks the remaining path. All
    /// intrinsics today are either one-segment (`print`) or
    /// two-segment (`Type.method`); nested-type intrinsics would
    /// extend the match arms without changing the shape.
    pub fn from_identifier(identifier: &Identifier) -> Option<Self> {
        match identifier.path() {
            [single] if single == "print" => Some(Self::Print),
            [receiver, method] => Self::from_pair(receiver, method),
            _ => None,
        }
    }

    fn from_pair(receiver: &str, method: &str) -> Option<Self> {
        if receiver == "Binary" {
            return BinaryMethod::from_source(method).map(Self::Binary);
        }
        if receiver == "Bits" {
            return BitsMethod::from_source(method).map(Self::Bits);
        }
        if receiver == "CPtr" {
            return CPtrMethod::from_source(method).map(Self::CPtr);
        }
        if receiver == "CString" {
            return CStringMethod::from_source(method).map(Self::CString);
        }
        if receiver == "Float" && method == "to_float32" {
            return Some(Self::NumericConvert(NumericConvert::FloatToFloat32));
        }
        if receiver == "Int"
            && let Some(target) = IntNarrowTarget::from_source(method)
        {
            return Some(Self::NumericConvert(NumericConvert::IntNarrow(target)));
        }
        if receiver == "Kernel" && method == "panic" {
            return Some(Self::Kernel(KernelMethod::Panic));
        }
        if receiver == "List" {
            return ListMethod::from_source(method).map(Self::List);
        }
        if receiver == "Map" {
            return MapMethod::from_source(method).map(Self::Map);
        }
        if receiver == "Process" {
            return ProcessMethod::from_source(method).map(Self::Process);
        }
        if receiver == "Ref" {
            return RefMethod::from_source(method).map(Self::Ref);
        }
        if receiver == "ReplyTo" {
            return ReplyToMethod::from_source(method).map(Self::ReplyTo);
        }
        if receiver == "Set" {
            return SetMethod::from_source(method).map(Self::Set);
        }
        if receiver == "Socket" {
            return SocketMethod::from_source(method).map(Self::Socket);
        }
        if receiver == "String"
            && let Some(m) = StringMethod::from_source(method)
        {
            return Some(Self::String(m));
        }
        if receiver == "UInt64" && method == "to_int" {
            return Some(Self::NumericConvert(NumericConvert::UInt64ToInt));
        }
        if method == "eq" {
            return EqualityImpl::from_receiver(receiver).map(Self::Equality);
        }
        if method == "format" {
            return DebugImpl::from_receiver(receiver).map(Self::Debug);
        }
        if method == "hash" {
            return HashImpl::from_receiver(receiver).map(Self::Hash);
        }
        if method == "parse" {
            return ParseTarget::from_source(receiver).map(Self::Parse);
        }
        if let Some(op) = BitOp::from_source(method) {
            return IntType::from_source(receiver).map(|ty| Self::Bitwise { ty, op });
        }
        None
    }
}

impl IntType {
    /// Whether the receiver's right-shift should preserve the sign
    /// bit. Signed integers (`Int`/`IntN`) use arithmetic shift;
    /// unsigned (`UIntN`) use logical shift.
    pub fn is_signed(self) -> bool {
        matches!(self, Self::Int | Self::Int8 | Self::Int16 | Self::Int32,)
    }

    fn from_source(s: &str) -> Option<Self> {
        Some(match s {
            "Int" => Self::Int,
            "Int8" => Self::Int8,
            "Int16" => Self::Int16,
            "Int32" => Self::Int32,
            "UInt8" => Self::UInt8,
            "UInt16" => Self::UInt16,
            "UInt32" => Self::UInt32,
            "UInt64" => Self::UInt64,
            _ => return None,
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::Int => "Int",
            Self::Int8 => "Int8",
            Self::Int16 => "Int16",
            Self::Int32 => "Int32",
            Self::UInt8 => "UInt8",
            Self::UInt16 => "UInt16",
            Self::UInt32 => "UInt32",
            Self::UInt64 => "UInt64",
        }
    }
}

impl FloatType {
    fn from_source(s: &str) -> Option<Self> {
        Some(match s {
            "Float" => Self::Float,
            "Float32" => Self::Float32,
            _ => return None,
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::Float => "Float",
            Self::Float32 => "Float32",
        }
    }
}

impl DebugImpl {
    /// Map a receiver-type name (`"Bool"`, `"Int"`, `"Float"`,
    /// `"Float32"`, `"Int8"`, …) to the matching impl cell.
    /// Returns `None` for receivers outside the four families
    /// (e.g. `String`, struct types) — `String.format` is pure
    /// Koja and user types route through the synthesized
    /// `impl Debug` blocks.
    pub fn from_receiver(receiver: &str) -> Option<Self> {
        Some(match receiver {
            "Bool" => Self::Bool,
            "Float" => Self::Float,
            "Float32" => Self::Float32,
            other => Self::Int(IntType::from_source(other)?),
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::Bool => "Bool",
            Self::Float => "Float",
            Self::Float32 => "Float32",
            Self::Int(ty) => ty.segment(),
        }
    }
}

impl EqualityImpl {
    /// Map a receiver-type name (`"Bool"`, `"Int"`, `"Float"`,
    /// `"String"`, …) to the matching impl cell. Returns `None` for
    /// receivers outside the four families (`Bool` / numeric /
    /// `String` / struct types).
    pub fn from_receiver(receiver: &str) -> Option<Self> {
        if let Some(ty) = FloatType::from_source(receiver) {
            return Some(Self::Float(ty));
        }
        Some(match receiver {
            "Bool" => Self::Bool,
            "String" => Self::String,
            other => Self::Int(IntType::from_source(other)?),
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::Bool => "Bool",
            Self::Float(ty) => ty.segment(),
            Self::Int(ty) => ty.segment(),
            Self::String => "String",
        }
    }
}

impl HashImpl {
    /// Mirror of [`EqualityImpl::from_receiver`] — the two families
    /// share the same receiver surface today.
    pub fn from_receiver(receiver: &str) -> Option<Self> {
        Some(match receiver {
            "Bool" => Self::Bool,
            "String" => Self::String,
            other => Self::Int(IntType::from_source(other)?),
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::Bool => "Bool",
            Self::Int(ty) => ty.segment(),
            Self::String => "String",
        }
    }
}

impl BitOp {
    fn from_source(s: &str) -> Option<Self> {
        Some(match s {
            "band" => Self::Band,
            "bnot" => Self::Bnot,
            "bor" => Self::Bor,
            "bsl" => Self::Bsl,
            "bsr" => Self::Bsr,
            "bxor" => Self::Bxor,
            _ => return None,
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::Band => "band",
            Self::Bnot => "bnot",
            Self::Bor => "bor",
            Self::Bsl => "bsl",
            Self::Bsr => "bsr",
            Self::Bxor => "bxor",
        }
    }
}

impl KernelMethod {
    fn segment(self) -> &'static str {
        match self {
            Self::Panic => "panic",
        }
    }
}

impl CPtrMethod {
    fn from_source(s: &str) -> Option<Self> {
        Some(match s {
            "alloc" => Self::Alloc,
            "free" => Self::Free,
            "null" => Self::Null,
            "null?" => Self::NullQ,
            "offset" => Self::Offset,
            "read" => Self::Read,
            "to_binary" => Self::ToBinary,
            "to_string" => Self::ToString,
            "write" => Self::Write,
            _ => return None,
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::Alloc => "alloc",
            Self::Free => "free",
            Self::Null => "null",
            Self::NullQ => "null?",
            Self::Offset => "offset",
            Self::Read => "read",
            Self::ToBinary => "to_binary",
            Self::ToString => "to_string",
            Self::Write => "write",
        }
    }
}

impl CStringMethod {
    fn from_source(s: &str) -> Option<Self> {
        Some(match s {
            "to_string" => Self::ToString,
            _ => return None,
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::ToString => "to_string",
        }
    }
}

impl BinaryMethod {
    fn from_source(s: &str) -> Option<Self> {
        Some(match s {
            "at" => Self::At,
            "byte_size" => Self::ByteSize,
            "ptr" => Self::Ptr,
            "slice" => Self::Slice,
            "to_bits" => Self::ToBits,
            "to_string" => Self::ToString,
            _ => return None,
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::At => "at",
            Self::ByteSize => "byte_size",
            Self::Ptr => "ptr",
            Self::Slice => "slice",
            Self::ToBits => "to_bits",
            Self::ToString => "to_string",
        }
    }
}

impl BitsMethod {
    fn from_source(s: &str) -> Option<Self> {
        Some(match s {
            "to_binary" => Self::ToBinary,
            _ => return None,
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::ToBinary => "to_binary",
        }
    }
}

impl ListMethod {
    fn from_source(s: &str) -> Option<Self> {
        Some(match s {
            "append" => Self::Append,
            "concat" => Self::Concat,
            "empty?" => Self::EmptyQ,
            "from_list" => Self::FromList,
            "get" => Self::Get,
            "length" => Self::Length,
            "new" => Self::New,
            "pop" => Self::Pop,
            "replace_at" => Self::ReplaceAt,
            "slice" => Self::Slice,
            _ => return None,
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::Append => "append",
            Self::Concat => "concat",
            Self::EmptyQ => "empty?",
            Self::FromList => "from_list",
            Self::Get => "get",
            Self::Length => "length",
            Self::New => "new",
            Self::Pop => "pop",
            Self::ReplaceAt => "replace_at",
            Self::Slice => "slice",
        }
    }
}

impl MapMethod {
    fn from_source(s: &str) -> Option<Self> {
        Some(match s {
            "empty?" => Self::EmptyQ,
            "from_map" => Self::FromMap,
            "get" => Self::Get,
            "has?" => Self::HasQ,
            "length" => Self::Length,
            "new" => Self::New,
            "put" => Self::Put,
            "remove" => Self::Remove,
            _ => return None,
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::EmptyQ => "empty?",
            Self::FromMap => "from_map",
            Self::Get => "get",
            Self::HasQ => "has?",
            Self::Length => "length",
            Self::New => "new",
            Self::Put => "put",
            Self::Remove => "remove",
        }
    }
}

impl RefMethod {
    fn from_source(s: &str) -> Option<Self> {
        Some(match s {
            "alive?" => Self::AliveQ,
            "call" => Self::Call,
            "cast" => Self::Cast,
            "kill" => Self::Kill,
            "self_ref" => Self::SelfRef,
            "send_after" => Self::SendAfter,
            "signal" => Self::Signal,
            _ => return None,
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::AliveQ => "alive?",
            Self::Call => "call",
            Self::Cast => "cast",
            Self::Kill => "kill",
            Self::SelfRef => "self_ref",
            Self::SendAfter => "send_after",
            Self::Signal => "signal",
        }
    }
}

impl ProcessMethod {
    fn from_source(s: &str) -> Option<Self> {
        Some(match s {
            "demonitor" => Self::Demonitor,
            "monitor" => Self::Monitor,
            "parent" => Self::Parent,
            _ => return None,
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::Demonitor => "demonitor",
            Self::Monitor => "monitor",
            Self::Parent => "parent",
        }
    }
}

impl ReplyToMethod {
    fn from_source(s: &str) -> Option<Self> {
        Some(match s {
            "send" => Self::Send,
            _ => return None,
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::Send => "send",
        }
    }
}

impl SetMethod {
    fn from_source(s: &str) -> Option<Self> {
        Some(match s {
            "empty?" => Self::EmptyQ,
            "from_list" => Self::FromList,
            "has?" => Self::HasQ,
            "insert" => Self::Insert,
            "length" => Self::Length,
            "new" => Self::New,
            "remove" => Self::Remove,
            _ => return None,
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::EmptyQ => "empty?",
            Self::FromList => "from_list",
            Self::HasQ => "has?",
            Self::Insert => "insert",
            Self::Length => "length",
            Self::New => "new",
            Self::Remove => "remove",
        }
    }
}

impl SocketMethod {
    fn from_source(s: &str) -> Option<Self> {
        Some(match s {
            "recv_from" => Self::RecvFrom,
            "resolve" => Self::Resolve,
            _ => return None,
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::RecvFrom => "recv_from",
            Self::Resolve => "resolve",
        }
    }
}

impl NumericConvert {
    /// The `Receiver.method` rendering used by [`Display`].
    fn path(self) -> String {
        match self {
            Self::FloatToFloat32 => "Float.to_float32".to_string(),
            Self::IntNarrow(target) => format!("Int.to_{}", target.suffix()),
            Self::UInt64ToInt => "UInt64.to_int".to_string(),
        }
    }
}

impl IntNarrowTarget {
    fn from_source(method: &str) -> Option<Self> {
        Some(match method {
            "to_int8" => Self::Int8,
            "to_int16" => Self::Int16,
            "to_int32" => Self::Int32,
            "to_uint8" => Self::UInt8,
            "to_uint16" => Self::UInt16,
            "to_uint32" => Self::UInt32,
            "to_uint64" => Self::UInt64,
            _ => return None,
        })
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Int8 => "int8",
            Self::Int16 => "int16",
            Self::Int32 => "int32",
            Self::UInt8 => "uint8",
            Self::UInt16 => "uint16",
            Self::UInt32 => "uint32",
            Self::UInt64 => "uint64",
        }
    }
}

impl ParseTarget {
    fn from_source(s: &str) -> Option<Self> {
        Some(match s {
            "Float" => Self::Float,
            "Int" => Self::Int,
            _ => return None,
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::Float => "Float",
            Self::Int => "Int",
        }
    }
}

impl StringMethod {
    fn from_source(s: &str) -> Option<Self> {
        Some(match s {
            "byte_length" => Self::ByteLength,
            "get" => Self::Get,
            "length" => Self::Length,
            "slice" => Self::Slice,
            "to_binary" => Self::ToBinary,
            "to_cstring" => Self::ToCstring,
            _ => return None,
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::ByteLength => "byte_length",
            Self::Get => "get",
            Self::Length => "length",
            Self::Slice => "slice",
            Self::ToBinary => "to_binary",
            Self::ToCstring => "to_cstring",
        }
    }
}

impl fmt::Display for IRIntrinsicId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Binary(m) => write!(f, "Binary.{}", m.segment()),
            Self::Bits(m) => write!(f, "Bits.{}", m.segment()),
            Self::Bitwise { ty, op } => write!(f, "{}.{}", ty.segment(), op.segment()),
            Self::CPtr(m) => write!(f, "CPtr.{}", m.segment()),
            Self::CString(m) => write!(f, "CString.{}", m.segment()),
            Self::Debug(impl_) => write!(f, "{}.format", impl_.segment()),
            Self::Equality(impl_) => write!(f, "{}.eq", impl_.segment()),
            Self::Hash(impl_) => write!(f, "{}.hash", impl_.segment()),
            Self::Kernel(m) => write!(f, "Kernel.{}", m.segment()),
            Self::List(m) => write!(f, "List.{}", m.segment()),
            Self::Map(m) => write!(f, "Map.{}", m.segment()),
            Self::NumericConvert(convert) => f.write_str(&convert.path()),
            Self::Parse(target) => write!(f, "{}.parse", target.segment()),
            Self::Print => f.write_str("print"),
            Self::Process(m) => write!(f, "Process.{}", m.segment()),
            Self::Ref(m) => write!(f, "Ref.{}", m.segment()),
            Self::ReplyTo(m) => write!(f, "ReplyTo.{}", m.segment()),
            Self::Set(m) => write!(f, "Set.{}", m.segment()),
            Self::Socket(m) => write!(f, "Socket.{}", m.segment()),
            Self::String(m) => write!(f, "String.{}", m.segment()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(path: &[&str]) -> Identifier {
        Identifier::new("Global", path.iter().map(|s| s.to_string()).collect())
    }

    fn assert_round_trip(path: &[&str], expected: IRIntrinsicId, expected_display: &str) {
        let parsed = IRIntrinsicId::from_identifier(&id(path))
            .unwrap_or_else(|| panic!("expected `{path:?}` to parse"));
        assert_eq!(parsed, expected, "parsed variant for {path:?}");
        assert_eq!(parsed.to_string(), expected_display, "Display for {path:?}",);
    }

    #[test]
    fn print_is_top_level_one_segment() {
        assert_round_trip(&["print"], IRIntrinsicId::Print, "print");
    }

    #[test]
    fn kernel_panic_round_trips() {
        assert_round_trip(
            &["Kernel", "panic"],
            IRIntrinsicId::Kernel(KernelMethod::Panic),
            "Kernel.panic",
        );
    }

    #[test]
    fn cptr_methods_cover_the_full_surface() {
        for (method, variant) in [
            ("alloc", CPtrMethod::Alloc),
            ("free", CPtrMethod::Free),
            ("null", CPtrMethod::Null),
            ("null?", CPtrMethod::NullQ),
            ("offset", CPtrMethod::Offset),
            ("read", CPtrMethod::Read),
            ("write", CPtrMethod::Write),
            ("to_binary", CPtrMethod::ToBinary),
            ("to_string", CPtrMethod::ToString),
        ] {
            assert_round_trip(
                &["CPtr", method],
                IRIntrinsicId::CPtr(variant),
                &format!("CPtr.{method}"),
            );
        }
    }

    #[test]
    fn list_methods_cover_the_full_surface() {
        for (method, variant) in [
            ("append", ListMethod::Append),
            ("concat", ListMethod::Concat),
            ("empty?", ListMethod::EmptyQ),
            ("from_list", ListMethod::FromList),
            ("get", ListMethod::Get),
            ("length", ListMethod::Length),
            ("new", ListMethod::New),
            ("pop", ListMethod::Pop),
            ("replace_at", ListMethod::ReplaceAt),
            ("slice", ListMethod::Slice),
        ] {
            assert_round_trip(
                &["List", method],
                IRIntrinsicId::List(variant),
                &format!("List.{method}"),
            );
        }
    }

    #[test]
    fn map_methods_cover_the_full_surface() {
        for (method, variant) in [
            ("empty?", MapMethod::EmptyQ),
            ("from_map", MapMethod::FromMap),
            ("get", MapMethod::Get),
            ("has?", MapMethod::HasQ),
            ("length", MapMethod::Length),
            ("new", MapMethod::New),
            ("put", MapMethod::Put),
            ("remove", MapMethod::Remove),
        ] {
            assert_round_trip(
                &["Map", method],
                IRIntrinsicId::Map(variant),
                &format!("Map.{method}"),
            );
        }
    }

    #[test]
    fn set_methods_cover_the_full_surface() {
        for (method, variant) in [
            ("empty?", SetMethod::EmptyQ),
            ("from_list", SetMethod::FromList),
            ("has?", SetMethod::HasQ),
            ("insert", SetMethod::Insert),
            ("length", SetMethod::Length),
            ("new", SetMethod::New),
            ("remove", SetMethod::Remove),
        ] {
            assert_round_trip(
                &["Set", method],
                IRIntrinsicId::Set(variant),
                &format!("Set.{method}"),
            );
        }
    }

    #[test]
    fn socket_methods_cover_the_full_surface() {
        for (method, variant) in [
            ("recv_from", SocketMethod::RecvFrom),
            ("resolve", SocketMethod::Resolve),
        ] {
            assert_round_trip(
                &["Socket", method],
                IRIntrinsicId::Socket(variant),
                &format!("Socket.{method}"),
            );
        }
    }

    #[test]
    fn ref_methods_cover_the_full_surface() {
        for (method, variant) in [
            ("alive?", RefMethod::AliveQ),
            ("call", RefMethod::Call),
            ("cast", RefMethod::Cast),
            ("kill", RefMethod::Kill),
            ("self_ref", RefMethod::SelfRef),
            ("send_after", RefMethod::SendAfter),
            ("signal", RefMethod::Signal),
        ] {
            assert_round_trip(
                &["Ref", method],
                IRIntrinsicId::Ref(variant),
                &format!("Ref.{method}"),
            );
        }
    }

    #[test]
    fn process_methods_cover_the_full_surface() {
        for (method, variant) in [
            ("demonitor", ProcessMethod::Demonitor),
            ("monitor", ProcessMethod::Monitor),
            ("parent", ProcessMethod::Parent),
        ] {
            assert_round_trip(
                &["Process", method],
                IRIntrinsicId::Process(variant),
                &format!("Process.{method}"),
            );
        }
    }

    #[test]
    fn reply_to_send_round_trips() {
        assert_round_trip(
            &["ReplyTo", "send"],
            IRIntrinsicId::ReplyTo(ReplyToMethod::Send),
            "ReplyTo.send",
        );
    }

    #[test]
    fn unknown_ref_or_reply_to_methods_return_none() {
        assert!(IRIntrinsicId::from_identifier(&id(&["Ref", "frobnicate"])).is_none());
        assert!(IRIntrinsicId::from_identifier(&id(&["ReplyTo", "shout"])).is_none());
    }

    #[test]
    fn string_methods_cover_the_full_surface() {
        for (method, variant) in [
            ("byte_length", StringMethod::ByteLength),
            ("get", StringMethod::Get),
            ("length", StringMethod::Length),
            ("slice", StringMethod::Slice),
            ("to_binary", StringMethod::ToBinary),
            ("to_cstring", StringMethod::ToCstring),
        ] {
            assert_round_trip(
                &["String", method],
                IRIntrinsicId::String(variant),
                &format!("String.{method}"),
            );
        }
    }

    #[test]
    fn debug_format_covers_bool_float_int_and_int_widths() {
        for ty_str in [
            "Int", "Int8", "Int16", "Int32", "UInt8", "UInt16", "UInt32", "UInt64",
        ] {
            let ty = IntType::from_source(ty_str).unwrap();
            assert_round_trip(
                &[ty_str, "format"],
                IRIntrinsicId::Debug(DebugImpl::Int(ty)),
                &format!("{ty_str}.format"),
            );
        }
        assert_round_trip(
            &["Bool", "format"],
            IRIntrinsicId::Debug(DebugImpl::Bool),
            "Bool.format",
        );
        assert_round_trip(
            &["Float", "format"],
            IRIntrinsicId::Debug(DebugImpl::Float),
            "Float.format",
        );
        assert_round_trip(
            &["Float32", "format"],
            IRIntrinsicId::Debug(DebugImpl::Float32),
            "Float32.format",
        );
    }

    #[test]
    fn debug_excludes_string_receiver() {
        assert!(
            IRIntrinsicId::from_identifier(&id(&["String", "format"])).is_none(),
            "String.format ships a pure-Koja body, not an intrinsic",
        );
    }

    #[test]
    fn equality_and_hash_cover_bool_int_and_string() {
        for ty_str in [
            "Int", "Int8", "Int16", "Int32", "UInt8", "UInt16", "UInt32", "UInt64",
        ] {
            let ty = IntType::from_source(ty_str).unwrap();
            assert_round_trip(
                &[ty_str, "eq"],
                IRIntrinsicId::Equality(EqualityImpl::Int(ty)),
                &format!("{ty_str}.eq"),
            );
            assert_round_trip(
                &[ty_str, "hash"],
                IRIntrinsicId::Hash(HashImpl::Int(ty)),
                &format!("{ty_str}.hash"),
            );
        }
        for ty_str in ["Float", "Float32"] {
            let ty = FloatType::from_source(ty_str).unwrap();
            assert_round_trip(
                &[ty_str, "eq"],
                IRIntrinsicId::Equality(EqualityImpl::Float(ty)),
                &format!("{ty_str}.eq"),
            );
        }
        assert_round_trip(
            &["Bool", "eq"],
            IRIntrinsicId::Equality(EqualityImpl::Bool),
            "Bool.eq",
        );
        assert_round_trip(
            &["Bool", "hash"],
            IRIntrinsicId::Hash(HashImpl::Bool),
            "Bool.hash",
        );
        assert_round_trip(
            &["String", "eq"],
            IRIntrinsicId::Equality(EqualityImpl::String),
            "String.eq",
        );
        assert_round_trip(
            &["String", "hash"],
            IRIntrinsicId::Hash(HashImpl::String),
            "String.hash",
        );
    }

    #[test]
    fn bitwise_table_is_eight_types_by_six_ops() {
        for ty_str in [
            "Int", "Int8", "Int16", "Int32", "UInt8", "UInt16", "UInt32", "UInt64",
        ] {
            let ty = IntType::from_source(ty_str).unwrap();
            for (op_str, op) in [
                ("band", BitOp::Band),
                ("bor", BitOp::Bor),
                ("bxor", BitOp::Bxor),
                ("bsl", BitOp::Bsl),
                ("bsr", BitOp::Bsr),
                ("bnot", BitOp::Bnot),
            ] {
                assert_round_trip(
                    &[ty_str, op_str],
                    IRIntrinsicId::Bitwise { ty, op },
                    &format!("{ty_str}.{op_str}"),
                );
            }
        }
    }

    #[test]
    fn bitwise_excludes_bool_receiver() {
        assert!(
            IRIntrinsicId::from_identifier(&id(&["Bool", "band"])).is_none(),
            "`Bool.band` has no impl in stdlib; `Bool` is outside `IntType`",
        );
    }

    #[test]
    fn parse_routes_int_and_float() {
        assert_round_trip(
            &["Int", "parse"],
            IRIntrinsicId::Parse(ParseTarget::Int),
            "Int.parse",
        );
        assert_round_trip(
            &["Float", "parse"],
            IRIntrinsicId::Parse(ParseTarget::Float),
            "Float.parse",
        );
    }

    #[test]
    fn unknown_segmentation_returns_none() {
        assert!(IRIntrinsicId::from_identifier(&id(&["unknown"])).is_none());
        assert!(IRIntrinsicId::from_identifier(&id(&["Kernel", "elope"])).is_none());
        assert!(IRIntrinsicId::from_identifier(&id(&["CPtr", "frobnicate"])).is_none());
        assert!(
            IRIntrinsicId::from_identifier(&id(&["Outer", "Inner", "method"])).is_none(),
            "three-segment paths aren't part of today's intrinsic surface",
        );
    }

    #[test]
    fn int_type_signedness_matches_naming() {
        assert!(IntType::Int.is_signed());
        assert!(IntType::Int8.is_signed());
        assert!(IntType::Int32.is_signed());
        assert!(!IntType::UInt8.is_signed());
        assert!(!IntType::UInt32.is_signed());
        assert!(!IntType::UInt64.is_signed());
    }
}
