//! Resolved `match` expressions: the result-type strategy plus the lowered
//! patterns for each arm. Bodies and guards are not lowered into IR; emission
//! walks the resolved patterns in lockstep with the source AST `MatchArm`s.
//!
//! ## Invariant: result-type strategy is a typecheck decision
//!
//! `ResolvedMatchType` is computed purely from the typed AST during lowering
//! (each arm body's last-expression `Type`, plus the surrounding function's
//! `return_type_hint` for union members). Emission does not re-derive it from
//! observed LLVM types -- if the lowered strategy disagrees with what
//! emission actually produces, that is reported as an error rather than
//! silently dropping the value.
//!
//! ## Invariant: "no value" is structural, not a strategy
//!
//! There is intentionally no `Void` variant. A match produces no value when
//! every reachable arm either terminates (e.g. `return`) or ends in a
//! non-expression statement; that's an emission-time observation
//! (`incoming.is_empty()`), not a typecheck decision.

use expo_ast::types::Type;

use crate::resolved::patterns::ResolvedPattern;

/// The result-type strategy for a match expression: how arm values are
/// combined into the final phi.
pub enum ResolvedMatchType {
    /// All arm values share `ty`; emit them directly into the phi.
    Direct { ty: Type },
    /// Arm types differ but all are members of `target` (a known union);
    /// each arm value is wrapped into the union before being phi'd.
    UnionWrap { target: Type },
}

/// A `match` expression after lowering. Carries the subject's resolved type,
/// the lowered patterns (one per source arm, in order), and the result-type
/// strategy. Guards and bodies stay in the AST -- emission walks `arms`
/// alongside this struct.
pub struct ResolvedMatch {
    /// The subject's Expo type after lowering's fallback inference.
    pub subject_ty: Type,
    /// One resolved pattern per source arm, in source order.
    pub patterns: Vec<ResolvedPattern>,
    /// How arm values combine in the result phi.
    pub result_ty: ResolvedMatchType,
}
