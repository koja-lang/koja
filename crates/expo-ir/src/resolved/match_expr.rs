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

use crate::blocks::{IRBlockId, IRTerminator};
use crate::values::{IRInstruction, IRValueId};

/// The result-type strategy for a match expression: how arm values are
/// combined into the final phi.
pub enum ResolvedMatchType {
    /// All arm values share `ty`; emit them directly into the phi.
    Direct { ty: Type },
    /// Arm types differ but all are members of `target` (a known union);
    /// each arm value is wrapped into the union before being phi'd.
    UnionWrap { target: Type },
}

/// One arm of an [`IRMatch`]: a check (pattern + binding-setup +
/// guard, all in one instruction stream producing the cond-branch
/// operand) and a body (statement list with a declared exit
/// terminator). Mirrors the shape of
/// [`crate::resolved::conditionals::IRCondArm`].
///
/// Slice 5b retired the synthetic-bridge from 5a:
///
/// - `check_instructions` is a self-contained instruction stream that
///   produces the arm's i1. It contains, in source order:
///   pattern-test primitives ([`IRInstruction::PatternTagEq`] /
///   [`IRInstruction::PatternLiteralEq`] /
///   [`IRInstruction::PatternProjectVariantField`] /
///   [`IRInstruction::PatternUnionPayloadPtr`] /
///   [`IRInstruction::PatternBinaryMatch`]), pattern bindings
///   ([`IRInstruction::PatternBindFromPtr`], which register entries
///   into `Compiler.fn_state.variables`), AND/OR fusion via
///   [`IRInstruction::BinaryOp`] (`BoolAnd` / `BoolOr`), and the
///   guard's lowered operand stream when a guard is present.
///   `check_terminator.cond` is the [`IROperand`] produced by this
///   stream.
///
/// Why bindings live in `check_instructions` and not the body block:
/// Expo guards reference pattern bindings (`Some(v) when v > 0`),
/// and guards evaluate before the cond branch. The codegen walker
/// wraps each arm in a `Compiler.fn_state.variables` clone/restore
/// so the bindings scope to the arm rather than leaking to
/// subsequent arms. The 5b lift moved binding *setup* into IR
/// (visible as [`IRInstruction::PatternBindFromPtr`]); the per-arm
/// *scoping* stays in codegen because the variables map carries
/// LLVM-typed allocas not exposed at the IR surface.
///
/// The arm's two blocks:
///
/// - `check_block` -- holds `check_instructions` followed by
///   `check_terminator` (`CondBranch { cond: <pattern+guard operand>,
///   then: body_block, otherwise: <next> }`, where `<next>` is the
///   next arm's `check_block` for non-final arms or the surrounding
///   match's `fallthrough_block` for the last arm).
/// - `body_block` -- runs when the cond branch fires. Holds
///   `body_stmts` (AST stub); declared exit is `body_terminator` =
///   `Branch(merge_block)`. Emission honors the terminator only when
///   the body has not already self-terminated (e.g. via early `return`
///   / `panic`).
///
/// Unlike [`crate::resolved::conditionals::IRCondArm`], the first arm
/// here does **not** double as the construct's implicit entry: every
/// arm (including arm 0) gets a fresh LLVM `check_block`. The implicit
/// entry runs the subject expression and the alloca that stores it,
/// then branches into `arms[0].check_block`.
pub struct IRMatchArm {
    pub body_block: IRBlockId,
    pub body_stmts: Vec<Statement>,
    pub body_terminator: IRTerminator,
    pub check_block: IRBlockId,
    pub check_instructions: Vec<IRInstruction>,
    pub check_terminator: IRTerminator,
}

/// Outcome of lowering a `match` expression. N-arm structure mirroring
/// [`crate::resolved::conditionals::IRCond`]: arms chain via each arm's
/// `check_terminator` `otherwise` slot pointing at the next arm's
/// `check_block`, with the tail arm's `otherwise` pointing at
/// `fallthrough_block` (the all-patterns-failed landing pad).
///
/// Blocks:
///
/// - `arms[*].check_block` -- one fresh LLVM block per arm. The walker
///   branches into `arms[0].check_block` from the subject-evaluation
///   prologue.
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
/// value) into before running per-arm `check_instructions`. Each arm's
/// pattern primitives reference it via [`IROperand::Local`]
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
