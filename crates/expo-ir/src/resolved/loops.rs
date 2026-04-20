//! Resolved loop metadata: the decisions a `for` loop's lowering makes
//! about which `Enumeration` impl to dispatch to, with all package-aware
//! type-key resolution and protocol/method lookups already performed.
//!
//! Lowering (in [`crate::lower::loops`]) consumes the AST iterable's
//! `Type` and produces a [`ResolvedEnumerable`]. Emission then mints the
//! `length` / `get` symbol names from `mangled_type`, computes the LLVM
//! type for `elem_type`, and walks the indexed-while desugaring -- no
//! protocol-impl lookups, no signature substitution.

use expo_typecheck::types::Type;

/// Outcome of resolving an `Enumeration` impl for a `for` loop's iterable.
///
/// Carries everything emission needs to dispatch into the impl: the
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
    pub mangled_type: String,
    /// Concrete type arguments applied to the base type, in declaration
    /// order. Empty for non-generic `Enumeration` impls.
    pub type_args: Vec<Type>,
}
