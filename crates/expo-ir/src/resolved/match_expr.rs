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

use crate::blocks::{IRBasicBlock, IRBlockId};
use crate::values::{IROperand, IRValueId};

/// The result-type strategy for a match expression: how arm values are
/// combined into the final phi.
pub enum ResolvedMatchType {
    /// All arm values share `ty`; emit them directly into the phi.
    Direct { ty: Type },
    /// Arm types differ but all are members of `target` (a known union);
    /// each arm value is wrapped into the union before being phi'd.
    UnionWrap { target: Type },
}

/// One arm of an [`IRMatch`]: a per-arm check sub-CFG (pattern tests,
/// binding setup, guard) followed by a body block. Mirrors the shape
/// of [`crate::resolved::conditionals::IRCondArm`] but generalized: an
/// arm's check is an arbitrary sub-CFG rather than a single block.
///
/// ## Per-arm check sub-CFG (`check_blocks`)
///
/// `check_blocks[0]` is the arm entry -- the target of the previous
/// arm's failure edge (or the match prologue's branch into the
/// dispatch chain for arm 0).
///
/// Today's flat case (patterns that don't deref payloads -- `Wildcard`,
/// `Bind`, `LiteralEq`, `EnumUnit`, `PatternBinaryMatch`, plus
/// `UnionMember` whose payload-bind always succeeds without further
/// gating) is `check_blocks.len() == 1`: a single block whose
/// instructions test the pattern + guard, followed by a `CondBranch`
/// to `body_block` on success or to the next arm's entry / the match's
/// `fallthrough_block` on failure.
///
/// Constructor patterns (`EnumStruct`, `EnumTuple`, and any pattern
/// containing them recursively) produce `check_blocks.len() >= 2`.
/// Each tag-discriminated test gets its own block so that the
/// payload-projection instructions only execute on the success branch
/// of the enclosing tag check. This is what fixes the GAPS
/// "Nested enum pattern matching with literal payloads" entry: the
/// payload-load that used to deref uninitialized memory when the outer
/// tag didn't match now lives in a successor block that's only entered
/// when that tag check succeeds.
///
/// ### Sub-CFG invariants
///
/// - Every block in `check_blocks` belongs to this arm. No other
///   arm's check refers into them.
/// - Every block's terminator targets either (a) another block in
///   `check_blocks`, (b) the arm's own `body_block`, (c) the next
///   arm's `check_blocks[0]`, or (d) the match's `fallthrough_block`.
///   The sub-CFG never escapes the match.
/// - Failure edges from interior blocks point directly at the next
///   arm's entry / `fallthrough_block`. There is no per-arm "fail"
///   collector block.
///
/// ## Pattern bindings
///
/// `check_blocks` carries [`crate::values::IRInstruction::PatternBindFromPtr`]
/// instructions in the same blocks where the binding becomes
/// well-defined: payload-bound bindings live in the payload block,
/// not the entry, so a binding never registers when its enclosing tag
/// check failed. Guards reference these bindings; the guard's lowered
/// operand stream lives in whatever block becomes the "open" block at
/// the end of pattern lowering and is `BoolAnd`-fused with the
/// pattern's final i1 there.
///
/// The codegen walker wraps each arm (every block in `check_blocks`
/// plus the body) in a `Compiler.fn_state.variables` clone/restore so
/// per-arm bindings scope to the arm rather than leaking forward.
/// The clone/restore stays in codegen because the variables map
/// carries LLVM-typed allocas that aren't part of the IR surface.
///
/// ## Body
///
/// `body` runs when the final cond-branch in `check_blocks` fires.
/// Full IR basic block; declared exit is `Branch(merge_block)` (or
/// the body's own terminator when it ends in `return` / `break`).
///
/// `trailing_value` is the captured trailing-expression operand
/// (after the per-arm UnionWrap pre-stage when `IRMatch.result_ty`
/// is UnionWrap and the arm's trailing type isn't already a union).
/// `Some(...)` iff the body ends in a `Statement::Expr` and no early
/// terminator fired at lowering time. The inline merge-phi assembly
/// in `expo_codegen` reads this operand from the per-arm `value_map`
/// after `body.instructions` runs to seed the phi's incoming pair
/// `(value, body_end_bb)`. See [`IRMatch`] for the surrounding
/// strategy.
pub struct IRMatchArm {
    pub body: IRBasicBlock,
    pub check_blocks: Vec<IRBasicBlock>,
    pub trailing_value: Option<IROperand>,
}

/// Outcome of lowering a `match` expression. N-arm structure mirroring
/// [`crate::resolved::conditionals::IRCond`]: arms chain via the
/// failure edges in each arm's `check_blocks` sub-CFG pointing at the
/// next arm's `check_blocks[0]`, with the tail arm's failure edges
/// pointing at `fallthrough_block` (the all-patterns-failed landing
/// pad).
///
/// Blocks:
///
/// - `arms[*].check_blocks` -- per-arm sub-CFG (entry block plus any
///   payload-gated successors required by the pattern). The walker
///   branches into `arms[0].check_blocks[0].id` from the
///   subject-evaluation prologue.
/// - `arms[*].body_block` -- one fresh LLVM block per arm.
/// - `fallthrough_block` -- runs when no arm's pattern matched. Always
///   present (matches legacy `emit_match`'s `fallthrough_bb`); the phi
///   in `merge_block` registers a zero-valued `undef` incoming from this
///   block when value-producing.
/// - `merge_block` -- landing point. Emission positions there after
///   walking every arm + the fallthrough; if the lowered `result_ty`
///   strategy yields matching LLVM types across all value-producing
///   arms, an inline-synthesized phi materializes a `TypedValue`.
///
/// Value-merge contract (matches legacy `emit_match` semantics):
///
/// - All reachable arms produced a value with matching LLVM type
///   under the lowered strategy -> `Ok(Some(TypedValue))`.
/// - Zero arms produced (every arm terminated or ended in a non-Expr
///   statement) -> `Ok(None)` (structural void).
/// - Some-but-not-all arms produced -> `Ok(None)` (matches legacy:
///   no unified phi shape, drop the value silently).
/// - All arms produced but LLVM types disagree under the lowered
///   strategy -> `Err`.
///
/// Per-arm UnionWrap (when `result_ty == UnionWrap` and the arm's
/// trailing type isn't already the union) is pre-staged as the last
/// instruction of `arms[i].body.instructions`; the dest is what
/// `arms[i].trailing_value` references. The merge phi itself is
/// still synthesized inline by [`expo_codegen`]'s
/// `assemble_match_phi` (Slice 2 deferred full pre-staging because
/// `match` arm bodies can self-terminate via `panic` calls --
/// indistinguishable from a plain `Statement::Expr` at lowering --
/// and a pre-staged phi has no clean way to elide the resulting
/// dead incoming).
///
/// `subject_value` is the SSA slot the emit walker stuffs the match
/// subject's pointer into before running per-arm pattern checks.
pub struct IRMatch {
    pub arms: Vec<IRMatchArm>,
    pub fallthrough_block: IRBlockId,
    pub merge_block: IRBlockId,
    pub result_ty: ResolvedMatchType,
    pub subject_ty: Type,
    pub subject_value: IRValueId,
}
