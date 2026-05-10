//! Typed dispatch id for `@intrinsic`-annotated functions. Replaces
//! the prior free-form `id: String` (joined from the function's
//! identifier path) with an exhaustive enum so both backends
//! ([`expo_alpha_ir_llvm`] and [`expo_alpha_ir_eval`]) match a
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

use expo_ast::identifier::Identifier;

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
    Equality(EqualityImpl),
    Hash(HashImpl),
    Kernel(KernelMethod),
    Parse(ParseTarget),
    /// Transitional. The script-mode test fixture's `@intrinsic fn
    /// print(s: String)` plus the project-mode auto-print of
    /// `main`'s tail value. Goes away when the `Debug` protocol
    /// displaces both.
    Print,
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
    ByteSize,
    Ptr,
    ToBits,
    ToString,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitsMethod {
    ToBinary,
}

/// Receiver shape for `Equality.eq` impls. Today the only impls are
/// on the integral types, so the variant nests an [`IntegralType`].
/// Adding `String`, `Float`, or aggregate equality later is a new
/// top-level variant — keeping the seam visible avoids a future
/// rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EqualityImpl {
    Int(IntegralType),
}

/// Same shape as [`EqualityImpl`] for the `Hash.hash` family. The
/// two are kept distinct (rather than collapsed into a single
/// "primitive impl" enum) so divergent future variants — e.g. a
/// `String` `Hash` impl with no equivalent on `Equality` — don't
/// pollute the other family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashImpl {
    Int(IntegralType),
}

/// Receivers that participate in `Equality.eq` / `Hash.hash`.
/// Wider than [`IntType`] because `Bool` carries both protocols
/// but no bitwise operators (no `Bool.band`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegralType {
    Bool,
    Int,
    Int8,
    Int16,
    Int32,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
}

/// Receivers for the 48-cell `Bitwise` family. Exactly the integer
/// subset of [`IntegralType`]; `Bool` is excluded because the
/// stdlib's `impl Bitwise` blocks don't cover it.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitOp {
    Band,
    Bnot,
    Bor,
    Bsl,
    Bsr,
    Bxor,
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
    /// typo'd `@intrinsic` decls fail at parse → check time, not
    /// at codegen.
    ///
    /// Strips the package prefix and walks the remaining path. All
    /// alpha intrinsics today are either one-segment (`print`) or
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
        if receiver == "Kernel" && method == "panic" {
            return Some(Self::Kernel(KernelMethod::Panic));
        }
        if method == "eq" {
            return IntegralType::from_source(receiver)
                .map(|ty| Self::Equality(EqualityImpl::Int(ty)));
        }
        if method == "hash" {
            return IntegralType::from_source(receiver).map(|ty| Self::Hash(HashImpl::Int(ty)));
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

impl IntegralType {
    fn from_source(s: &str) -> Option<Self> {
        Some(match s {
            "Bool" => Self::Bool,
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
            Self::Bool => "Bool",
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
            "byte_size" => Self::ByteSize,
            "ptr" => Self::Ptr,
            "to_bits" => Self::ToBits,
            "to_string" => Self::ToString,
            _ => return None,
        })
    }

    fn segment(self) -> &'static str {
        match self {
            Self::ByteSize => "byte_size",
            Self::Ptr => "ptr",
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

impl fmt::Display for IRIntrinsicId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Binary(m) => write!(f, "Binary.{}", m.segment()),
            Self::Bits(m) => write!(f, "Bits.{}", m.segment()),
            Self::Bitwise { ty, op } => write!(f, "{}.{}", ty.segment(), op.segment()),
            Self::CPtr(m) => write!(f, "CPtr.{}", m.segment()),
            Self::CString(m) => write!(f, "CString.{}", m.segment()),
            Self::Equality(EqualityImpl::Int(ty)) => write!(f, "{}.eq", ty.segment()),
            Self::Hash(HashImpl::Int(ty)) => write!(f, "{}.hash", ty.segment()),
            Self::Kernel(m) => write!(f, "Kernel.{}", m.segment()),
            Self::Parse(target) => write!(f, "{}.parse", target.segment()),
            Self::Print => f.write_str("print"),
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
    fn equality_and_hash_share_the_integral_axis() {
        for ty_str in [
            "Bool", "Int", "Int8", "Int16", "Int32", "UInt8", "UInt16", "UInt32", "UInt64",
        ] {
            let ty = IntegralType::from_source(ty_str).unwrap();
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
