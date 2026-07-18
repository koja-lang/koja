//! Sub-passes of the typecheck phase, run in order by
//! [`crate::check_program`]:
//!
//! - `synthesize::derive_debug` and `synthesize::derive_equality`:
//!   append derived impls before binding.
//! - [`collect::collect_file_decls`] +
//!   [`collect::collect_file_impls`]: register every top-level decl,
//!   then every `impl` block (cross-file two-pass).
//! - [`collect::validate_nested_types`] and
//!   [`aliases::validate_aliases`]: validate declarations against the
//!   complete registry.
//! - [`lift_signatures::lift_signatures`][]: stamp
//!   [`crate::registry::FunctionSignature`]s and lifted struct /
//!   enum / protocol payloads.
//! - [`visibility::check_signature_leaks`]: reject private types in
//!   public signatures.
//! - [`synthesize::synthesize_program`]: surface-shape AST rewrites
//!   (today: `for` desugar).
//! - [`resolve::resolve_file`]: populate `Resolution` /
//!   `Expr.resolution` on every node.
//! - [`borrows::check_file`]: reject `CPtr.borrow` results escaping
//!   their borrowing statement.
//! - [`seal::seal_ast`]: assert sealed-AST invariants.
//!
//! Errors return before seal, so seal only sees successful trees.

pub(crate) mod aliases;
pub(crate) mod borrows;
pub(crate) mod collect;
pub(crate) mod lift_signatures;
pub(crate) mod local_scope;
pub(crate) mod resolve;
pub(crate) mod seal;
pub(crate) mod synthesize;
pub(crate) mod unify;
pub(crate) mod visibility;

pub use unify::{Substitution, substitute};
