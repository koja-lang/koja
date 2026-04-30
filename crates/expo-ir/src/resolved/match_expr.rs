//! Resolved metadata for `match` expressions.
//!
//! Slice 3 dissolved the per-construct IR types (`IRMatch`,
//! `IRMatchArm`) -- recursive lowering builds the match CFG directly
//! into a [`crate::CFGBuilder`] (see
//! [`crate::Lowerer::lower_match_expr`]).
//!
//! [`ResolvedMatchType`] survives as the strategy decision the
//! lowering consults to decide whether arm values feed the merge phi
//! directly or via an [`crate::values::IRInstruction::UnionWrap`]
//! pre-stage.
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

/// The result-type strategy for a match expression: how arm values are
/// combined into the final phi.
#[derive(Clone)]
pub enum ResolvedMatchType {
    /// All arm values share `ty`; emit them directly into the phi.
    Direct { ty: Type },
    /// Arm types differ but all are members of `target` (a known union);
    /// each arm value is wrapped into the union before being phi'd.
    UnionWrap { target: Type },
}
