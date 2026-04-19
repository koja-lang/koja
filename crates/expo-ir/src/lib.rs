//! ExpoIR: home for the LLVM-free decision types and lowering helpers that
//! sit between the typed AST and codegen backends.
//!
//! Today the crate hosts the `Resolved*` decision-type vocabulary
//! ([`resolved`]) and the freestanding lowering helpers ([`lower`]) that
//! produce them, plus shared semantic state ([`FnLowerState`],
//! [`TypeLayouts`]) and transitional identities ([`identity::VariantId`]).
//! The full SIL-style instruction containers (function, basic block,
//! instruction sequence) are intentionally undefined in code -- their shape
//! will be discovered bottom-up during the lowering/emission split, driven
//! by what `Resolved*` consumers need to be stitched together. See
//! `expo/design/EXPOIR.md` for design intent and current wave status.

mod fn_state;
pub mod identity;
pub mod lower;
pub mod resolved;
mod type_layouts;
pub mod util;

pub use fn_state::FnLowerState;
pub use identity::VariantId;
pub use type_layouts::TypeLayouts;
