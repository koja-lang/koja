//! Panic messages both backends must print verbatim. A message that
//! depends on the faulting operator lives on [`crate::IRBinOp`]
//! instead, next to its surface spelling.

/// `ArithmeticError` for negating a signed type's minimum value.
pub const NEG_OVERFLOW_MESSAGE: &str = "integer overflow in unary -";

/// `CPtr.read` loaded a NaN or infinity into a finite-only float type.
pub const CPTR_READ_NON_FINITE_MESSAGE: &str = "non-finite float read by CPtr.read";

/// An `@extern "C"` call returned a NaN or infinity into a finite-only
/// float type. `c_name` is the callee's C symbol.
pub fn extern_non_finite_message(c_name: &str) -> String {
    format!("non-finite float returned by {c_name}")
}
