//! Stable identities for IR-level entities. Today these are tuple-of-strings
//! transitional types; in the IR end-state (Phase 5+) they become opaque
//! interned IDs (e.g. `EnumId(u32)`, `(EnumId, u8)`). Call sites consume
//! these via `VariantId::new(...)` and never see the inner representation,
//! so the eventual swap is internal.

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct VariantId {
    pub enum_mangled: String,
    pub variant_name: String,
}

impl VariantId {
    pub fn new(enum_mangled: impl Into<String>, variant_name: impl Into<String>) -> Self {
        Self {
            enum_mangled: enum_mangled.into(),
            variant_name: variant_name.into(),
        }
    }
}
