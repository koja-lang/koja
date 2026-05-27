//! Constructive expression forms. Everything in here builds an
//! `Expr` out of a delimited token shape — strings, lists/maps, paren
//! groups, closures, binary literals, struct/enum constructors. The
//! Pratt loop in [`crate::expr`] dispatches to these from
//! `parse_prefix`.
//!
//! Modules:
//! - `binary` — `<<...>>` binary literal expressions
//! - `closure` — block-form `fn(...) ... end` and short `expr -> expr` closures
//! - `list` — `[...]` list / map literals and `(...)` paren expressions
//! - `string` — quoted strings + multiline triple-quoted dedent
//! - `type_construction` — struct construction, enum variant construction

pub(crate) mod binary;
pub(crate) mod closure;
pub(crate) mod list;
pub(crate) mod string;
pub(crate) mod type_construction;
