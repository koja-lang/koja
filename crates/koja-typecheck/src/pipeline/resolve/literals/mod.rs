//! Typecheck for the AST's literal-shaped expressions.
//!
//! Today's surface: list literals (`[a, b, c]`), map literals
//! (`["k": v, ...]`), and binary literals (`<<...>>`). Each
//! per-shape resolver lives in its own file. The carrier-protocol
//! mechanics that list and map share (the
//! `<carrier>.<from_method>(<canonical-literal>)` synthesis when
//! the surrounding hint demands a non-default conformer) live in
//! [`carrier`]. Axis-type inference (the per-slot
//! "hint-or-floor-or-diagnose" walk shared between list elements
//! and map keys/values) lives in [`axis`].
//!
//! Future literal-protocol families (`IntLiteral<T>` for `123`,
//! `FloatLiteral<T>` for `1.0`, `BinaryLiteral<T>` for `<<...>>`
//! once it grows a protocol) slot in here as new files. Each one
//! provides its own `CarrierSpec` plus literal-shape work
//! (taking the inner kind apart, inferring per-axis types when
//! relevant) and forwards through `dispatch_via_carrier` for the
//! shared rewrite.

mod axis;
mod binary;
mod carrier;
mod list;
mod map;

pub(super) use binary::resolve_binary_literal;
pub(crate) use binary::{SegmentKind, resolve_segment};
pub(super) use list::resolve_list_literal;
pub(super) use map::resolve_map_literal;
