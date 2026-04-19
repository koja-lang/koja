//! Decision types extracted from codegen: semantic choices separated from
//! backend emission. Each module contains types that the lowering pass
//! produces and the emission pass consumes.

pub mod closures;
pub mod constants;
pub mod construction;
pub mod enums;
pub mod fields;
pub mod match_expr;
pub mod methods;
pub mod ops;
pub mod patterns;
pub mod strings;
