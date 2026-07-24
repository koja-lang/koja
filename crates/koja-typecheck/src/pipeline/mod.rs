//! Sub-passes of the typecheck phase, run in order by
//! [`crate::check_program`]:
//!
//! - [`desugar::desugar_packages`]: hoist lexically nested type
//!   declarations to qualified top-level items.
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
//! - [`deprecation::check_file`]: warn on uses of `@deprecated`
//!   declarations.
//! - [`seal::seal_ast`]: assert sealed-AST invariants.
//!
//! Errors return before seal, so seal only sees successful trees.

use koja_ast::ast::{Diagnostic, File};

pub(crate) mod aliases;
pub(crate) mod borrows;
pub(crate) mod collect;
pub(crate) mod deprecation;
pub(crate) mod desugar;
pub(crate) mod lift_signatures;
pub(crate) mod local_scope;
pub(crate) mod resolve;
pub(crate) mod seal;
pub(crate) mod synthesize;
pub(crate) mod unify;
pub(crate) mod visibility;

pub use resolve::types::peel_alias;
pub use unify::{Substitution, substitute};

/// Stamp diagnostics emitted since `start` with `file`'s owning path.
/// Per-file passes bracket their work with this so diagnostics carry
/// file attribution without threading paths through every emit site.
pub(crate) fn stamp_file_paths(diagnostics: &mut [Diagnostic], start: usize, file: &File) {
    let Some(path) = &file.path else { return };
    Diagnostic::stamp_paths(&mut diagnostics[start..], path);
}
