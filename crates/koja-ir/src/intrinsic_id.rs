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

/// Declare an intrinsic method enum together with its `from_source`
/// parser and `segment` display name, generated from one
/// variant-to-string table so the source spelling exists in exactly
/// one place.
macro_rules! intrinsic_methods {
    ($(
        $(#[$meta:meta])*
        $name:ident { $($variant:ident => $segment:literal),+ $(,)? }
    )+) => {$(
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $($variant,)+
        }

        impl $name {
            fn from_source(s: &str) -> Option<Self> {
                match s {
                    $($segment => Some(Self::$variant),)+
                    _ => None,
                }
            }

            fn segment(self) -> &'static str {
                match self {
                    $(Self::$variant => $segment,)+
                }
            }
        }
    )+};
}

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
    /// implicit hub widening. All return
    /// `Result<T, NumericConversionError>` and fail with `OutOfRange`
    /// when the receiver does not fit the target.
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
    RuntimeBlock(RuntimeBlockMethod),
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

intrinsic_methods! {
    BinaryMethod {
        At => "at",
        ByteSize => "byte_size",
        Slice => "slice",
        ToBits => "to_bits",
        ToString => "to_string",
    }

    BitsMethod {
        ToBinary => "to_binary",
    }

    BitOp {
        Band => "band",
        Bnot => "bnot",
        Bor => "bor",
        Bsl => "bsl",
        Bsr => "bsr",
        Bxor => "bxor",
    }

    CPtrMethod {
        Alloc => "alloc",
        Borrow => "borrow",
        Copy => "copy",
        Free => "free",
        Null => "null",
        NullQ => "null?",
        Offset => "offset",
        Read => "read",
        ToBinary => "to_binary",
        Write => "write",
    }

    CStringMethod {
        ToString => "to_string",
    }

    /// Float receivers for the float slice of `Equality`. Sibling of
    /// [`IntType`]; each width is its own emitter cell (LLVM picks
    /// f32 / f64 compare from the param's actual type).
    FloatType {
        Float => "Float",
        Float32 => "Float32",
    }

    /// Integer receivers for the 48-cell `Bitwise` family and the
    /// 8-cell integer slice of `Equality` / `Hash`. `Bool` and `String`
    /// are not included — they're siblings of [`EqualityImpl::Int`] /
    /// [`HashImpl::Int`] at the enum level.
    IntType {
        Int => "Int",
        Int8 => "Int8",
        Int16 => "Int16",
        Int32 => "Int32",
        UInt8 => "UInt8",
        UInt16 => "UInt16",
        UInt32 => "UInt32",
        UInt64 => "UInt64",
    }

    KernelMethod {
        Panic => "panic",
    }

    /// Methods on `List<T>`. The element type doesn't appear here because
    /// the IR carries it on the [`crate::IRFunction`] signature; backends
    /// monomorphize per element type from there.
    ListMethod {
        Append => "append",
        Concat => "concat",
        EmptyQ => "empty?",
        FromList => "from_list",
        Get => "get",
        Length => "length",
        New => "new",
        Pop => "pop",
        ReplaceAt => "replace_at",
        Slice => "slice",
    }

    /// Methods on `Map<K, V>`. Like [`ListMethod`], the key + value
    /// types don't appear here — both ride the [`crate::IRFunction`]
    /// signature, and backends specialize layouts per `(K, V)` pair
    /// from there.
    MapMethod {
        EmptyQ => "empty?",
        FromMap => "from_map",
        Get => "get",
        HasQ => "has?",
        Length => "length",
        New => "new",
        Put => "put",
        Remove => "remove",
    }

    ParseTarget {
        Float => "Float",
        Int => "Int",
    }

    /// `@intrinsic`-flagged statics on the `Process` protocol: `Monitor`
    /// registers the calling process as a watcher of a `Pid`, `Demonitor`
    /// retracts one, `Parent` reports the calling process's spawner.
    ProcessMethod {
        Demonitor => "demonitor",
        Monitor => "monitor",
        Parent => "parent",
    }

    /// `@intrinsic`-flagged methods on `Ref<M, R>` from
    /// [`koja/lib/global/src/process.koja`]. `Cast` / `Call` / `Signal` /
    /// `Kill` / `AliveQ` / `SendAfter` cover the public mailbox surface;
    /// `SelfRef` is the only zero-argument constructor (the others are
    /// receiver-bound).
    RefMethod {
        AliveQ => "alive?",
        Call => "call",
        Cast => "cast",
        Kill => "kill",
        SelfRef => "self_ref",
        SendAfter => "send_after",
        Signal => "signal",
    }

    /// `@intrinsic`-flagged method on `ReplyTo<R>`. Single-variant today
    /// (`send`); kept as a wrapper enum so adding a sibling later is a
    /// variant-add, not a shape change. Mirrors [`BitsMethod`] /
    /// [`KernelMethod`].
    ReplyToMethod {
        Send => "send",
    }

    RuntimeBlockMethod {
        AdoptBinary => "adopt_binary",
    }

    /// Methods on `Set<T>`. Same monomorphization story as
    /// [`ListMethod`] / [`MapMethod`] — the element type rides the
    /// [`crate::IRFunction`] signature.
    SetMethod {
        EmptyQ => "empty?",
        FromList => "from_list",
        HasQ => "has?",
        Insert => "insert",
        Length => "length",
        New => "new",
        Remove => "remove",
    }

    /// `@intrinsic`-flagged methods on `Socket` from
    /// [`koja/lib/net/src/net.koja`]. `RecvFrom` receives one
    /// datagram + sender address (suspending the process until the fd
    /// is readable); `Resolve` is a synchronous `getaddrinfo` shim.
    SocketMethod {
        LastError => "last_error",
        RecvFrom => "recv_from",
        Resolve => "resolve",
    }

    /// Methods on `String` flagged `@intrinsic` in
    /// [`crate::stdlib::string`]. Excludes `eq` / `hash`; those route
    /// through [`EqualityImpl::String`] / [`HashImpl::String`] alongside
    /// the other primitive impls.
    StringMethod {
        ByteLength => "byte_length",
        Get => "get",
        Length => "length",
        Slice => "slice",
        ToBinary => "to_binary",
        ToCstring => "to_cstring",
    }
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

/// One explicit numeric conversion method. `IntNarrow` covers the
/// seven `Int.to_*` methods (target width carried here, not read
/// back out of the return type). `UInt64ToInt` is the one sized
/// type with no implicit path to the hub. `FloatToFloat32` is the
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

    /// Dispatch on the receiver's namespace first, then fall through
    /// to the cross-receiver families (`eq` / `format` / `hash` /
    /// `parse` / bitwise) so receivers like `Int` and `String` can
    /// serve both.
    fn from_pair(receiver: &str, method: &str) -> Option<Self> {
        let namespaced = match receiver {
            "Binary" => BinaryMethod::from_source(method).map(Self::Binary),
            "Bits" => BitsMethod::from_source(method).map(Self::Bits),
            "CPtr" => CPtrMethod::from_source(method).map(Self::CPtr),
            "CString" => CStringMethod::from_source(method).map(Self::CString),
            "Float" if method == "to_float32" => {
                Some(Self::NumericConvert(NumericConvert::FloatToFloat32))
            }
            "Int" => IntNarrowTarget::from_source(method)
                .map(|target| Self::NumericConvert(NumericConvert::IntNarrow(target))),
            "Kernel" => KernelMethod::from_source(method).map(Self::Kernel),
            "List" => ListMethod::from_source(method).map(Self::List),
            "Map" => MapMethod::from_source(method).map(Self::Map),
            "Process" => ProcessMethod::from_source(method).map(Self::Process),
            "Ref" => RefMethod::from_source(method).map(Self::Ref),
            "ReplyTo" => ReplyToMethod::from_source(method).map(Self::ReplyTo),
            "RuntimeBlock" => RuntimeBlockMethod::from_source(method).map(Self::RuntimeBlock),
            "Set" => SetMethod::from_source(method).map(Self::Set),
            "Socket" => SocketMethod::from_source(method).map(Self::Socket),
            "String" => StringMethod::from_source(method).map(Self::String),
            "UInt64" if method == "to_int" => {
                Some(Self::NumericConvert(NumericConvert::UInt64ToInt))
            }
            _ => None,
        };
        if namespaced.is_some() {
            return namespaced;
        }
        match method {
            "eq" => EqualityImpl::from_receiver(receiver).map(Self::Equality),
            "format" => DebugImpl::from_receiver(receiver).map(Self::Debug),
            "hash" => HashImpl::from_receiver(receiver).map(Self::Hash),
            "parse" => ParseTarget::from_source(receiver).map(Self::Parse),
            _ => BitOp::from_source(method)
                .and_then(|op| IntType::from_source(receiver).map(|ty| Self::Bitwise { ty, op })),
        }
    }
}

impl IntType {
    /// Bit width of the receiver type. Shift counts must satisfy
    /// `0 <= n < bit_width()` or the shift traps.
    pub fn bit_width(self) -> u32 {
        match self {
            Self::Int | Self::UInt64 => 64,
            Self::Int8 | Self::UInt8 => 8,
            Self::Int16 | Self::UInt16 => 16,
            Self::Int32 | Self::UInt32 => 32,
        }
    }

    /// Whether the receiver's right-shift should preserve the sign
    /// bit. Signed integers (`Int`/`IntN`) use arithmetic shift;
    /// unsigned (`UIntN`) use logical shift.
    pub fn is_signed(self) -> bool {
        matches!(self, Self::Int | Self::Int8 | Self::Int16 | Self::Int32,)
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
    /// Panic message for a negative or width-and-larger shift
    /// count. Only `Bsl` / `Bsr` fault. Shared verbatim by both
    /// backends.
    pub fn shift_count_message(self) -> &'static str {
        match self {
            Self::Bsl => "shift count out of range in bsl",
            Self::Bsr => "shift count out of range in bsr",
            other => unreachable!("shift_count_message called with non-shift op {other:?}"),
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
            Self::RuntimeBlock(m) => write!(f, "RuntimeBlock.{}", m.segment()),
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

    /// Every namespaced `Receiver.method` intrinsic, spelled out
    /// independently of the `intrinsic_methods!` tables so a typo'd
    /// source string there fails here instead of round-tripping.
    #[test]
    fn namespaced_methods_cover_the_full_surface() {
        use IRIntrinsicId as Id;
        let cases: &[(&str, &str, Id)] = &[
            ("Binary", "at", Id::Binary(BinaryMethod::At)),
            ("Binary", "byte_size", Id::Binary(BinaryMethod::ByteSize)),
            ("Binary", "slice", Id::Binary(BinaryMethod::Slice)),
            ("Binary", "to_bits", Id::Binary(BinaryMethod::ToBits)),
            ("Binary", "to_string", Id::Binary(BinaryMethod::ToString)),
            ("Bits", "to_binary", Id::Bits(BitsMethod::ToBinary)),
            ("CPtr", "alloc", Id::CPtr(CPtrMethod::Alloc)),
            ("CPtr", "borrow", Id::CPtr(CPtrMethod::Borrow)),
            ("CPtr", "copy", Id::CPtr(CPtrMethod::Copy)),
            ("CPtr", "free", Id::CPtr(CPtrMethod::Free)),
            ("CPtr", "null", Id::CPtr(CPtrMethod::Null)),
            ("CPtr", "null?", Id::CPtr(CPtrMethod::NullQ)),
            ("CPtr", "offset", Id::CPtr(CPtrMethod::Offset)),
            ("CPtr", "read", Id::CPtr(CPtrMethod::Read)),
            ("CPtr", "to_binary", Id::CPtr(CPtrMethod::ToBinary)),
            ("CPtr", "write", Id::CPtr(CPtrMethod::Write)),
            ("CString", "to_string", Id::CString(CStringMethod::ToString)),
            ("Kernel", "panic", Id::Kernel(KernelMethod::Panic)),
            ("List", "append", Id::List(ListMethod::Append)),
            ("List", "concat", Id::List(ListMethod::Concat)),
            ("List", "empty?", Id::List(ListMethod::EmptyQ)),
            ("List", "from_list", Id::List(ListMethod::FromList)),
            ("List", "get", Id::List(ListMethod::Get)),
            ("List", "length", Id::List(ListMethod::Length)),
            ("List", "new", Id::List(ListMethod::New)),
            ("List", "pop", Id::List(ListMethod::Pop)),
            ("List", "replace_at", Id::List(ListMethod::ReplaceAt)),
            ("List", "slice", Id::List(ListMethod::Slice)),
            ("Map", "empty?", Id::Map(MapMethod::EmptyQ)),
            ("Map", "from_map", Id::Map(MapMethod::FromMap)),
            ("Map", "get", Id::Map(MapMethod::Get)),
            ("Map", "has?", Id::Map(MapMethod::HasQ)),
            ("Map", "length", Id::Map(MapMethod::Length)),
            ("Map", "new", Id::Map(MapMethod::New)),
            ("Map", "put", Id::Map(MapMethod::Put)),
            ("Map", "remove", Id::Map(MapMethod::Remove)),
            (
                "Process",
                "demonitor",
                Id::Process(ProcessMethod::Demonitor),
            ),
            ("Process", "monitor", Id::Process(ProcessMethod::Monitor)),
            ("Process", "parent", Id::Process(ProcessMethod::Parent)),
            ("Ref", "alive?", Id::Ref(RefMethod::AliveQ)),
            ("Ref", "call", Id::Ref(RefMethod::Call)),
            ("Ref", "cast", Id::Ref(RefMethod::Cast)),
            ("Ref", "kill", Id::Ref(RefMethod::Kill)),
            ("Ref", "self_ref", Id::Ref(RefMethod::SelfRef)),
            ("Ref", "send_after", Id::Ref(RefMethod::SendAfter)),
            ("Ref", "signal", Id::Ref(RefMethod::Signal)),
            ("ReplyTo", "send", Id::ReplyTo(ReplyToMethod::Send)),
            (
                "RuntimeBlock",
                "adopt_binary",
                Id::RuntimeBlock(RuntimeBlockMethod::AdoptBinary),
            ),
            ("Set", "empty?", Id::Set(SetMethod::EmptyQ)),
            ("Set", "from_list", Id::Set(SetMethod::FromList)),
            ("Set", "has?", Id::Set(SetMethod::HasQ)),
            ("Set", "insert", Id::Set(SetMethod::Insert)),
            ("Set", "length", Id::Set(SetMethod::Length)),
            ("Set", "new", Id::Set(SetMethod::New)),
            ("Set", "remove", Id::Set(SetMethod::Remove)),
            ("Socket", "last_error", Id::Socket(SocketMethod::LastError)),
            ("Socket", "recv_from", Id::Socket(SocketMethod::RecvFrom)),
            ("Socket", "resolve", Id::Socket(SocketMethod::Resolve)),
            (
                "String",
                "byte_length",
                Id::String(StringMethod::ByteLength),
            ),
            ("String", "get", Id::String(StringMethod::Get)),
            ("String", "length", Id::String(StringMethod::Length)),
            ("String", "slice", Id::String(StringMethod::Slice)),
            ("String", "to_binary", Id::String(StringMethod::ToBinary)),
            ("String", "to_cstring", Id::String(StringMethod::ToCstring)),
        ];
        for (receiver, method, expected) in cases {
            assert_round_trip(
                &[receiver, method],
                *expected,
                &format!("{receiver}.{method}"),
            );
        }
    }

    #[test]
    fn numeric_conversions_round_trip() {
        use IRIntrinsicId as Id;
        for (method, target) in [
            ("to_int8", IntNarrowTarget::Int8),
            ("to_int16", IntNarrowTarget::Int16),
            ("to_int32", IntNarrowTarget::Int32),
            ("to_uint8", IntNarrowTarget::UInt8),
            ("to_uint16", IntNarrowTarget::UInt16),
            ("to_uint32", IntNarrowTarget::UInt32),
            ("to_uint64", IntNarrowTarget::UInt64),
        ] {
            assert_round_trip(
                &["Int", method],
                Id::NumericConvert(NumericConvert::IntNarrow(target)),
                &format!("Int.{method}"),
            );
        }
        assert_round_trip(
            &["Float", "to_float32"],
            Id::NumericConvert(NumericConvert::FloatToFloat32),
            "Float.to_float32",
        );
        assert_round_trip(
            &["UInt64", "to_int"],
            Id::NumericConvert(NumericConvert::UInt64ToInt),
            "UInt64.to_int",
        );
    }

    #[test]
    fn unknown_ref_or_reply_to_methods_return_none() {
        assert!(IRIntrinsicId::from_identifier(&id(&["Ref", "frobnicate"])).is_none());
        assert!(IRIntrinsicId::from_identifier(&id(&["ReplyTo", "shout"])).is_none());
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
