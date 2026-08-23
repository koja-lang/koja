//! Typecheck for the AST's literal-shaped expressions.
//!
//! Scalar, list, and map literals can convert contextually through
//! literal protocols. Binary literals (`<<...>>`) and tuple literals
//! (`(a, b)`) keep their canonical types. Each per-shape resolver
//! lives in its own file. The carrier-protocol mechanics share the
//! `<carrier>.<from_method>(<canonical-literal>)` synthesis when
//! the surrounding hint demands a non-default conformer) live in
//! [`carrier`]. Axis-type inference (the per-slot
//! "hint-or-floor-or-diagnose" walk shared between list elements
//! and map keys/values) lives in [`axis`].
//!
//! Future literal-protocol families such as `BinaryLiteral` slot in
//! here and forward through `dispatch_via_carrier`.

mod axis;
mod binary;
mod carrier;
mod list;
mod map;
mod scalar;
mod tuple;

pub(super) use binary::resolve_binary_literal;
pub(crate) use binary::{SegmentKind, resolve_segment};
pub(super) use list::resolve_list_literal;
pub(super) use map::resolve_map_literal;
pub(super) use scalar::{is_scalar_literal, resolve_scalar_literal};
pub(super) use tuple::resolve_tuple_literal;
