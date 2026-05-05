//! Seal sub-pass: walks the merged [`crate::IRProgram`] /
//! [`crate::IRScript`] and asserts the sealed-IR invariants per the
//! [`COMPILER-NORTHSTAR.md`] contract. Panics on violation — seal
//! failures indicate compiler bugs in upstream sub-passes, not user
//! errors.
//!
//! [`COMPILER-NORTHSTAR.md`]: ../../design/COMPILER-NORTHSTAR.md
//!
//! Layout map:
//!
//! - [`program`] — entry point [`seal_program`] plus
//!   `seal_program_calls` (cross-function call-target lookup against
//!   the assembled `IRProgram`).
//! - [`script`] — entry point [`seal_script`] plus
//!   `seal_script_calls` (mirror for the script-shaped output, with
//!   `IRScript::function` as the lookup table).
//! - [`function`] — `seal_package` / `seal_function` / `seal_block` /
//!   `collect_block_ids`. Shared between the program and script
//!   paths because both shapes contain `IRPackage` fragments and
//!   both apply the same per-block invariants (operand
//!   defined-before-use, terminator-target validity, supported
//!   `ConstValue` / `IRType` widths).
//! - This module ([`mod.rs`]) — shared helpers used by all
//!   submodules: [`seal_panic`], [`require_supported_type`],
//!   [`require_supported_const`], [`require_defined`],
//!   [`instruction_operands`], [`terminator_operands`],
//!   [`terminator_targets`].
//!
//! Invariants asserted (program path):
//!
//! 1. The entry-point [`crate::IRSymbol`] resolves to a registered
//!    function.
//! 2. Every function in every package keys at its own symbol
//!    (`pkg.functions[sym].symbol == sym`).
//! 3. Every function has at least one basic block.
//! 4. Every basic-block id is unique within its function.
//! 5. Every operand referenced by an instruction or terminator points
//!    at a `ValueId` defined earlier in the same basic block.
//!    Parameter `ValueId`s are seeded into the entry block's defined
//!    set so body references to params are valid without a distinct
//!    "definition" instruction. Cross-block value flow doesn't appear
//!    in this slice — the assignment / locals slice introduces it via
//!    `StoreLocal` / `LoadLocal` (alloca-backed memory, not raw SSA
//!    use across blocks).
//! 6. Every `IRTerminator::Branch` / `CondBranch` target is a block
//!    that exists in the same function.
//! 7. Every `IRInstruction::Call`'s `callee` symbol resolves to a
//!    function that actually exists somewhere in the `IRProgram` /
//!    `IRScript`.
//! 8. **Transient slice invariant**: every [`ConstValue`] and
//!    [`IRType`] that flows through the IR is one of `Bool`,
//!    `Float64`, `Int64`, `String`, or `Unit`. The narrower /
//!    unsigned / `Float32` width variants (`Int8` / `Int16` /
//!    `Int32` / `UInt8` / `UInt16` / `UInt32` / `UInt64` /
//!    `Float32`) exist in the IR vocabulary so future stdlib stub
//!    expansion + literal width inference can stamp them without
//!    reshuffling, but they're forbidden until those upstream pieces
//!    land. Loosen this invariant when adding `Int8` / `Float32` /
//!    etc. to the stdlib stubs. Applies to function return types,
//!    parameter types, and every value-flow [`IRType`] alike.
//!
//! The script path ([`seal_script`]) re-asserts (3)–(8) on the
//! implicit-function shape ([`crate::IRScript::blocks`] +
//! [`crate::IRScript::return_type`]), and re-asserts (7) using
//! [`crate::IRScript::packages`] as the call-target lookup.

use std::collections::BTreeSet;

use crate::function::{IRBlockId, IRInstruction, IRTerminator};
use crate::types::{ConstValue, IRType, ValueId};

mod function;
mod program;
mod script;

pub(crate) use program::seal_program;
pub(crate) use script::seal_script;

/// Transient slice invariant: only `Bool` / `Float64` / `Int64` /
/// `String` / `Unit` flow through the IR. See module docstring
/// invariant 8.
pub(super) fn require_supported_type(ty: &IRType, location: &dyn Fn() -> String) {
    match ty {
        IRType::Bool | IRType::Float64 | IRType::Int64 | IRType::String | IRType::Unit => {}
        other => seal_panic(&format!(
            "{}: IRType `{other:?}` is not yet supported (alpha slice admits only \
             Bool / Float64 / Int64 / String / Unit until stdlib stub expansion lands)",
            location(),
        )),
    }
}

pub(super) fn require_supported_const(value: &ConstValue, location: &dyn Fn() -> String) {
    match value {
        ConstValue::Bool(_)
        | ConstValue::Float64(_)
        | ConstValue::Int64(_)
        | ConstValue::String(_)
        | ConstValue::Unit => {}
        other => seal_panic(&format!(
            "{}: ConstValue `{other:?}` is not yet supported (alpha slice admits only \
             Bool / Float64 / Int64 / String / Unit until stdlib stub expansion lands)",
            location(),
        )),
    }
}

pub(super) fn instruction_operands(inst: &IRInstruction) -> Vec<ValueId> {
    match inst {
        IRInstruction::BinaryOp { lhs, rhs, .. } => vec![*lhs, *rhs],
        IRInstruction::Call { args, .. } => args.clone(),
        IRInstruction::Const { .. } => vec![],
        IRInstruction::UnaryOp { operand, .. } => vec![*operand],
    }
}

pub(super) fn terminator_operands(term: &IRTerminator) -> Vec<ValueId> {
    match term {
        IRTerminator::Branch(_) => vec![],
        IRTerminator::CondBranch { cond, .. } => vec![*cond],
        IRTerminator::Return { value } => value.iter().copied().collect(),
    }
}

pub(super) fn terminator_targets(term: &IRTerminator) -> Vec<IRBlockId> {
    match term {
        IRTerminator::Branch(target) => vec![*target],
        IRTerminator::CondBranch {
            then_block,
            else_block,
            ..
        } => vec![*then_block, *else_block],
        IRTerminator::Return { .. } => vec![],
    }
}

pub(super) fn require_defined(value: ValueId, owner: &str, defined: &BTreeSet<ValueId>) {
    if !defined.contains(&value) {
        seal_panic(&format!(
            "{owner} references value `{value}` before it is defined",
        ));
    }
}

pub(super) fn seal_panic(message: &str) -> ! {
    panic!("alpha IR seal violation: {message}");
}
