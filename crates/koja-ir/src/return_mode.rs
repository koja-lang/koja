//! IR-side return mode: whether a callable hands back fresh heap the
//! caller owns ([`ReturnMode::Owned`]) or a view it must never free
//! ([`ReturnMode::Borrowed`]). Distinct from `koja_ast::ast::ReturnMode`;
//! lowering converts the AST verdict (computed by typecheck and stored
//! on `FunctionSignature`) into this IR vocabulary.
//!
//! The mode rides on [`crate::types::IRType::Function`] via
//! [`FnReturnMode`] so closure values carry it to indirect call sites.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

/// Ownership of a callable's result.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ReturnMode {
    /// Aliases an input or a static; the call site must not free it.
    /// The conservative default.
    #[default]
    Borrowed,
    /// Fresh heap (or ownership moved through a `move` parameter); the
    /// call site owns and may drop it.
    Owned,
}

impl From<koja_ast::ast::ReturnMode> for ReturnMode {
    fn from(mode: koja_ast::ast::ReturnMode) -> Self {
        match mode {
            koja_ast::ast::ReturnMode::Borrowed => Self::Borrowed,
            koja_ast::ast::ReturnMode::Owned => Self::Owned,
        }
    }
}

/// [`ReturnMode`] carried on [`crate::types::IRType::Function`] as
/// metadata that does **not** participate in type identity: `fn(T) -> U`
/// is structurally one type regardless of whether a given callee returns
/// owned or borrowed, so equality / hashing / ordering ignore the mode.
/// Read at indirect call sites to decide result ownership.
#[derive(Clone, Copy, Debug, Default)]
pub struct FnReturnMode(pub ReturnMode);

impl PartialEq for FnReturnMode {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for FnReturnMode {}

impl PartialOrd for FnReturnMode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FnReturnMode {
    fn cmp(&self, _: &Self) -> Ordering {
        Ordering::Equal
    }
}

impl Hash for FnReturnMode {
    fn hash<H: Hasher>(&self, _: &mut H) {}
}
