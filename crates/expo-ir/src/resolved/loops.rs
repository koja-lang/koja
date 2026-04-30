//! Resolved metadata for loop constructs.
//!
//! Slice 3 dissolved the per-construct IR types (`IRLoop`, `IRWhile`,
//! `IRFor`) -- recursive lowering builds the loop CFG directly into a
//! [`crate::CFGBuilder`] (see [`crate::Lowerer::lower_loop`] /
//! [`crate::Lowerer::lower_while`] / [`crate::Lowerer::lower_for`]).
//!
//! [`ResolvedEnumerable`] survives as the input type for the
//! pre-codegen elaboration pass that expands the
//! [`crate::values::IRInstruction::ForLoopStub`] placeholder into the
//! iterator-protocol multi-block desugar (`length()` / `get()` /
//! `Option` unwrap / pattern bind / `idx++`).

use expo_typecheck::types::Type;

use crate::identity::MonomorphizedTypeIdentifier;

/// Outcome of resolving an `Enumeration` impl for a `for` loop's iterable.
///
/// Carries everything elaboration needs to dispatch into the impl: the
/// mangled type name (= symbol prefix for `length` / `get`), the base
/// type name and type-args (for triggering monomorphization of the impl
/// methods), and the element's Expo type (for picking the LLVM payload
/// type and binding the loop variable).
pub struct ResolvedEnumerable {
    /// Source-level base type name, unmangled (e.g. `List`, `Vec`,
    /// `String`). Used as the type key for `monomorphize_impl_method`.
    pub base: String,
    /// Expo type of one element (the payload of the `Option` returned by
    /// `get`). Used to compute the LLVM type for the loop binding.
    pub elem_type: Type,
    /// Mangled, monomorphized type name (e.g. `List_$Int32$`). Used as
    /// the symbol prefix for the `length` / `get` function lookups.
    pub mangled_type: MonomorphizedTypeIdentifier,
    /// Concrete type arguments applied to the base type, in declaration
    /// order. Empty for non-generic `Enumeration` impls.
    pub type_args: Vec<Type>,
}
