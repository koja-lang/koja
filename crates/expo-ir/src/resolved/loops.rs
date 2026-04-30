//! Resolved loop metadata: structural metadata for `while` / `loop` /
//! `for` constructs (Wave 25 / Slice 6) plus the
//! `Enumeration`-dispatch decisions that `for` lowering makes about
//! which impl to dispatch to.
//!
//! ## Construct lifts (Slice 6)
//!
//! [`IRWhile`] / [`IRLoop`] / [`IRFor`] mirror the
//! [`crate::resolved::conditionals::IRCond`] shape-2 generalization:
//! lowering mints fresh [`IRBlockId`](crate::blocks::IRBlockId)s and
//! records the canonicalized control flow on a parallel-field IR
//! value; emission walks the value through the standard
//! `execute_instructions` + `emit_terminator` machinery. Bodies remain
//! AST `Vec<Statement>` stubs until Phase 4g (statement-level
//! lowering); the loop IR exposes the `exit_block` id so `break`
//! statements can resolve via the surrounding emit walker's
//! `loop_exit_stack` push/pop.
//!
//! `for` keeps the `iterable` AST + `binding_pattern` together (it is
//! a high-level construct whose iterator-protocol desugaring lives in
//! the emit walker; same precedent as
//! [`crate::values::IRInstruction::PatternBinaryMatch`] from Slice
//! 5b). [`ResolvedEnumerable`] continues to encode the impl-dispatch
//! decisions the desugaring needs.
//!
//! ## `Enumeration` dispatch
//!
//! Lowering (in [`crate::lower::loops`]) consumes the AST iterable's
//! `Type` and produces a [`ResolvedEnumerable`]. Emission then mints the
//! `length` / `get` symbol names from `mangled_type`, computes the LLVM
//! type for `elem_type`, and walks the indexed-while desugaring -- no
//! protocol-impl lookups, no signature substitution.

use expo_ast::ast::{Expr, Pattern};
use expo_typecheck::types::Type;

use crate::blocks::{IRBasicBlock, IRBlockId, IRTerminator};
use crate::identity::MonomorphizedTypeIdentifier;
use crate::values::{IRInstruction, IRValueId};

/// Outcome of lowering an infinite `loop ... end`. Two blocks: a
/// body block whose declared terminator is `Branch(body.id)` (the
/// back-edge -- overridden by `break` / `return` / `panic` in the
/// body), and an exit block that subsequent control flow lands in.
pub struct IRLoop {
    pub body: IRBasicBlock,
    pub exit_block: IRBlockId,
}

/// Outcome of lowering a `while cond ... end`. Three blocks:
///
/// - `header_block` -- holds `header_instructions` (the lowered
///   cond) followed by `header_terminator` (`CondBranch { cond,
///   then: body.id, otherwise: exit_block }`).
/// - `body` -- runs when the cond is truthy. Full IR block; declared
///   exit is `Branch(header_block)` (the back-edge), overridden by
///   `break` / `return` / `panic` in the body.
/// - `exit_block` -- landing point after the loop.
pub struct IRWhile {
    pub body: IRBasicBlock,
    pub exit_block: IRBlockId,
    pub header_block: IRBlockId,
    pub header_instructions: Vec<IRInstruction>,
    pub header_terminator: IRTerminator,
}

/// Outcome of lowering a `for binding in iterable ... end`. The
/// iterable + binding-pattern stay AST-stubbed because the
/// desugaring (`length()` + `get()` + `Option` unwrap + pattern
/// bind) is a multi-block algorithm that consults the LLVM type
/// registry; same precedent as
/// [`crate::values::IRInstruction::PatternBinaryMatch`].
///
/// Three blocks:
///
/// - `header_block` -- runs the index-vs-length comparison and
///   conditional branch. Materialized fully by the emit walker.
/// - `body` -- runs one iteration. Full IR block: the emit walker
///   prepends the per-iteration `get(idx)` + Option unwrap + pattern
///   bind, then the lowered body instructions, then increments idx
///   and branches back to `header_block` (or honors a `break` /
///   `return` already in body.terminator).
/// - `exit_block` -- landing point after the loop.
pub struct IRFor {
    pub binding_pattern: Pattern,
    pub body: IRBasicBlock,
    pub exit_block: IRBlockId,
    pub header_block: IRBlockId,
    pub idx_value: IRValueId,
    pub iterable: Expr,
    pub iterable_value: IRValueId,
}

/// Outcome of resolving an `Enumeration` impl for a `for` loop's iterable.
///
/// Carries everything emission needs to dispatch into the impl: the
/// mangled type name (= symbol prefix for `length` / `get`), the base
/// type name and type-args (for triggering monomorphization of the impl
/// methods), and the element's Expo type (for picking the LLVM payload
/// type and binding the loop variable).
pub struct ResolvedEnumerable {
    /// Source-level base type name, unmangled (e.g. `List`, `Vec`,
    /// `String`). Used as the type key for `monomorphize_impl_method`.
    pub base: String,
    /// Expo type of one element (the payload of the `Option` returned by
    /// `get`). Used to compute the LLVM type for the loop binding.
    pub elem_type: Type,
    /// Mangled, monomorphized type name (e.g. `List_$Int32$`). Used as
    /// the symbol prefix for the `length` / `get` function lookups.
    pub mangled_type: MonomorphizedTypeIdentifier,
    /// Concrete type arguments applied to the base type, in declaration
    /// order. Empty for non-generic `Enumeration` impls.
    pub type_args: Vec<Type>,
}
