//! Stable identities for IR-level entities. Today these are tuple-of-strings
//! transitional types; in the IR end-state (Phase 5+) they become opaque
//! interned IDs (e.g. `EnumId(u32)`, `(EnumId, u8)`). Call sites consume
//! these via `::new(...)` constructors and never see the inner representation,
//! so the eventual swap is internal.

use std::fmt;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct VariantIdentifier {
    pub enum_mangled: MonomorphizedTypeIdentifier,
    pub variant_name: String,
}

impl VariantIdentifier {
    pub fn new(
        enum_mangled: impl Into<MonomorphizedTypeIdentifier>,
        variant_name: impl Into<String>,
    ) -> Self {
        Self {
            enum_mangled: enum_mangled.into(),
            variant_name: variant_name.into(),
        }
    }
}

/// Identifies a monomorphized type instantiation (specialized generic
/// struct/enum, or a union). Today a `String` newtype wrapping the mangled
/// name; in Phase 5+ becomes an opaque interned identifier. Used as the key
/// into `LLVMTypeCache::monomorphized`, `TypeLayouts::mono_struct_info`,
/// `TypeLayouts::mono_enum_variants`, and the `enum_mangled` field of
/// `VariantIdentifier`.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct MonomorphizedTypeIdentifier(String);

impl MonomorphizedTypeIdentifier {
    pub fn new(mangled: impl AsRef<str>) -> Self {
        Self(mangled.as_ref().to_string())
    }

    /// Transitional accessor for code paths that still consume `&str`.
    /// Removed in Phase 5+.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for MonomorphizedTypeIdentifier {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for MonomorphizedTypeIdentifier {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for MonomorphizedTypeIdentifier {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&String> for MonomorphizedTypeIdentifier {
    fn from(s: &String) -> Self {
        Self::new(s.clone())
    }
}

impl From<&MonomorphizedTypeIdentifier> for MonomorphizedTypeIdentifier {
    fn from(id: &MonomorphizedTypeIdentifier) -> Self {
        id.clone()
    }
}

impl fmt::Display for MonomorphizedTypeIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifies a function in the LLVM module by its mangled name. Today a
/// `String` newtype; in Phase 5+ becomes an opaque interned identifier. Used
/// as the key into `Compiler::functions` and `Compiler::fn_ref_thunks`.
/// Covers non-generic functions, monomorphized generic functions, and
/// monomorphized impl methods.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FunctionIdentifier(String);

impl FunctionIdentifier {
    pub fn new(mangled: impl AsRef<str>) -> Self {
        Self(mangled.as_ref().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for FunctionIdentifier {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for FunctionIdentifier {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for FunctionIdentifier {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&String> for FunctionIdentifier {
    fn from(s: &String) -> Self {
        Self::new(s.clone())
    }
}

impl From<&FunctionIdentifier> for FunctionIdentifier {
    fn from(id: &FunctionIdentifier) -> Self {
        id.clone()
    }
}

impl fmt::Display for FunctionIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
