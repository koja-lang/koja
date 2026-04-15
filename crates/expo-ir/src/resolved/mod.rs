//! Decision types extracted from codegen: semantic choices separated from
//! backend emission. Each module contains types that the lowering pass
//! produces and the emission pass consumes.

pub mod closures;
pub mod constants;
pub mod enums;
pub mod fields;
pub mod methods;
pub mod ops;
pub mod strings;
