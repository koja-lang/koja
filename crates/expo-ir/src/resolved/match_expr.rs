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

use expo_ast::ast::Statement;
use expo_ast::types::Type;

use crate::blocks::{IRBasicBlock, IRBlockId, IRTerminator};
use crate::values::IRValueId;

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
/// `body_block` runs when the final cond-branch in `check_blocks`
/// fires. Holds `body_stmts` (AST stub); declared exit is
/// `body_terminator` = `Branch(merge_block)`. Emission honors the
/// terminator only when the body has not already self-terminated
/// (e.g. via early `return` / `panic`).
pub struct IRMatchArm {
    pub body_block: IRBlockId,
    pub body_stmts: Vec<Statement>,
    pub body_terminator: IRTerminator,
    pub check_blocks: Vec<IRBasicBlock>,
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
/// - All reachable arms produced a value with matching LLVM type under
///   the lowered strategy -> `Ok(Some(TypedValue))`.
/// - Zero arms produced (every arm terminated or ended in a non-Expr
///   statement) -> `Ok(None)` (structural void).
/// - Some-but-not-all arms produced -> `Ok(None)` (matches legacy:
///   no unified phi shape, drop the value silently).
/// - All arms produced but LLVM types disagree under the lowered
///   strategy -> `Err` (typecheck/codegen disagreement surfaced).
///
/// `merge_phi_dest` is pre-allocated at lowering time so a future slice
/// can fan the merge instruction out for inspection (e.g. ownership
/// analysis); the walker does not consult it today and synthesizes the
/// phi inline (mirrors `IRIfElse` / `IRCond`).
///
/// `subject_value` is the SSA slot the emit walker stuffs the match
/// subject's pointer (a freshly-allocated stack slot for the subject
/// value) into before running per-arm pattern checks. Each arm's
/// pattern primitives reference it via [`crate::values::IROperand::Local`]
/// (`subject_ptr` parameter). Slice 5b retired the prior approach of
/// passing the subject pointer in via an out-of-band `PointerValue`
/// argument to the walker.
///
/// `subject_ty` and `result_ty` are forwarded from the inner pattern
/// resolution; emission consults `result_ty` to pick the per-arm value
/// strategy (Direct vs UnionWrap).
pub struct IRMatch {
    pub arms: Vec<IRMatchArm>,
    pub fallthrough_block: IRBlockId,
    pub merge_block: IRBlockId,
    pub merge_phi_dest: IRValueId,
    pub result_ty: ResolvedMatchType,
    pub subject_ty: Type,
    pub subject_value: IRValueId,
}
