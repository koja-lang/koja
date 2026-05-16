//! Sub-passes of the typecheck phase, run in order by
//! [`crate::check_program`]:
//!
//! - [`collect::collect_file_decls`] +
//!   [`collect::collect_file_impls`] — register every top-level decl,
//!   then every `impl` block (cross-file two-pass).
//! - [`lift_signatures::lift_signatures`] — stamp
//!   [`crate::registry::FunctionSignature`]s and lifted struct /
//!   enum / protocol payloads.
//! - [`synthesize::synthesize_program`] — surface-shape AST rewrites
//!   (today: `for` desugar).
//! - [`resolve::resolve_file`] — populate `Resolution` /
//!   `Expr.resolution` on every node.
//! - [`seal::seal_ast`] — assert sealed-AST invariants.
//!
//! Pipeline contract: diagnostics short-circuit `check_program`
//! before `seal` runs, so `seal` only ever sees fully-resolved trees.

pub(crate) mod aliases;
pub(crate) mod collect;
pub(crate) mod lift_signatures;
pub(crate) mod local_scope;
pub(crate) mod resolve;
pub(crate) mod seal;
pub(crate) mod synthesize;
pub(crate) mod unify;

pub use unify::{Substitution, substitute};
