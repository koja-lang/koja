//! Seal sub-pass: walks the merged [`IRProgram`] / [`IRScript`] and
//! asserts the sealed-IR invariants per the [`COMPILER-NORTHSTAR.md`]
//! contract. Panics on violation — seal failures indicate compiler
//! bugs in upstream sub-passes, not user errors.
//!
//! [`COMPILER-NORTHSTAR.md`]: ../../design/COMPILER-NORTHSTAR.md
//!
//! Invariants asserted (program path):
//!
//! 1. The entry-point [`crate::IRSymbol`] resolves to a registered
//!    function.
//! 2. Every function in every package keys at its own symbol
//!    (`pkg.functions[sym].symbol == sym`).
//! 3. Every function has at least one basic block.
//! 4. Within each function: every value reference (instruction operand
//!    or terminator value) points at a `ValueId` defined earlier in
//!    the same function. Parameter `ValueId`s are seeded into the
//!    defined set before the first block, so body references to
//!    params are valid without having a distinct "definition"
//!    instruction.
//! 5. Every `IRInstruction::Call`'s `callee` symbol resolves to a
//!    function that actually exists somewhere in the `IRProgram` /
//!    `IRScript`.
//! 6. **Transient slice invariant**: every [`ConstValue`] and
//!    [`IRType`] that flows through the IR is one of `Bool`,
//!    `Int64`, or `Unit`. The narrower / unsigned width variants
//!    (`Int8` / `Int16` / `Int32` / `UInt8` / `UInt16` / `UInt32` /
//!    `UInt64`) exist in the IR vocabulary so future stdlib stub
//!    expansion + literal width inference can stamp them without
//!    reshuffling, but they're forbidden until those upstream pieces
//!    land. Loosen this invariant when adding `Int8` / etc. to the
//!    stdlib stubs. Applies to function return types, parameter
//!    types, and every value-flow [`IRType`] alike.
//!
//! The script path ([`seal_script`]) re-asserts (3)–(6) on the
//! implicit-function shape ([`IRScript::blocks`] +
//! [`IRScript::return_type`]), and re-asserts (5) using
//! [`IRScript::packages`] as the call-target lookup.

use std::collections::BTreeSet;

use crate::function::{IRBasicBlock, IRFunction, IRInstruction, IRTerminator};
use crate::script::IRScript;
use crate::types::{ConstValue, IRType, ValueId};
use crate::{IRProgram, package::IRPackage};

pub(crate) fn seal_program(program: &IRProgram) {
    if program.function(program.entry_point.mangled()).is_none() {
        seal_panic(&format!(
            "entry point `{}` not registered in any package",
            program.entry_point
        ));
    }
    for pkg in &program.packages {
        seal_package(pkg);
    }
    seal_program_calls(program);
}

pub(crate) fn seal_script(script: &IRScript) {
    for pkg in &script.packages {
        seal_package(pkg);
    }
    let owner = "script body";
    if script.blocks.is_empty() {
        seal_panic(&format!("{owner} has no basic blocks"));
    }
    require_supported_type(&script.return_type, &|| format!("{owner} return type"));
    let mut defined: BTreeSet<ValueId> = BTreeSet::new();
    for block in &script.blocks {
        seal_block(block, owner, &mut defined);
    }
    seal_script_calls(script);
}

fn seal_package(pkg: &IRPackage) {
    for (sym, function) in &pkg.functions {
        if sym != &function.symbol {
            seal_panic(&format!(
                "package `{}` keys function at `{sym}` but the function's own symbol is `{}`",
                pkg.package, function.symbol,
            ));
        }
        seal_function(function);
    }
}

fn seal_function(function: &IRFunction) {
    let owner = format!("function `{}`", function.symbol);
    if function.blocks.is_empty() {
        seal_panic(&format!("{owner} has no basic blocks"));
    }
    require_supported_type(&function.return_type, &|| format!("{owner} return type"));
    // Parameter `ValueId`s count as definitions for the purposes of
    // downstream operand references. Seed them first so the rest of
    // the block walk treats them as already-defined.
    let mut defined: BTreeSet<ValueId> = BTreeSet::new();
    for (index, param) in function.params.iter().enumerate() {
        require_supported_type(&param.ty, &|| {
            format!("{owner} parameter #{index} ({}) type", param.id)
        });
        if !defined.insert(param.id) {
            seal_panic(&format!(
                "{owner} lists duplicate parameter value `{}`",
                param.id,
            ));
        }
    }
    for block in &function.blocks {
        seal_block(block, &owner, &mut defined);
    }
}

fn seal_block(block: &IRBasicBlock, owner: &str, defined: &mut BTreeSet<ValueId>) {
    for inst in &block.instructions {
        for operand in instruction_operands(inst) {
            require_defined(operand, owner, defined);
        }
        if let IRInstruction::Const { value, .. } = inst {
            require_supported_const(value, &|| {
                format!("{owner} const instruction at {}", inst.dest())
            });
        }
        if !defined.insert(inst.dest()) {
            seal_panic(&format!("{owner} redefines value `{}`", inst.dest()));
        }
    }
    for operand in terminator_operands(&block.terminator) {
        require_defined(operand, owner, defined);
    }
}

/// Transient slice invariant: only `Bool` / `Int64` / `Unit` flow
/// through the IR. See module docstring invariant 6.
fn require_supported_type(ty: &IRType, location: &dyn Fn() -> String) {
    match ty {
        IRType::Bool | IRType::Int64 | IRType::Unit => {}
        other => seal_panic(&format!(
            "{}: IRType `{other:?}` is not yet supported (alpha slice admits only \
             Bool / Int64 / Unit until stdlib stub expansion lands)",
            location(),
        )),
    }
}

fn require_supported_const(value: &ConstValue, location: &dyn Fn() -> String) {
    match value {
        ConstValue::Bool(_) | ConstValue::Int64(_) | ConstValue::Unit => {}
        other => seal_panic(&format!(
            "{}: ConstValue `{other:?}` is not yet supported (alpha slice admits only \
             Bool / Int64 / Unit until stdlib stub expansion lands)",
            location(),
        )),
    }
}

fn instruction_operands(inst: &IRInstruction) -> Vec<ValueId> {
    match inst {
        IRInstruction::BinaryOp { lhs, rhs, .. } => vec![*lhs, *rhs],
        IRInstruction::Call { args, .. } => args.clone(),
        IRInstruction::Const { .. } => vec![],
        IRInstruction::UnaryOp { operand, .. } => vec![*operand],
    }
}

fn terminator_operands(term: &IRTerminator) -> Vec<ValueId> {
    match term {
        IRTerminator::Return { value } => value.iter().copied().collect(),
    }
}

fn require_defined(value: ValueId, owner: &str, defined: &BTreeSet<ValueId>) {
    if !defined.contains(&value) {
        seal_panic(&format!(
            "{owner} references value `{value}` before it is defined",
        ));
    }
}

/// Cross-function check: every `IRInstruction::Call` must name a
/// callee that exists as a registered function in the IRProgram. Lower
/// dereferences the callee id through the typecheck registry, so a
/// missing target here would indicate either a registry / IRProgram
/// drift or a genuine lowering bug — both compiler issues.
fn seal_program_calls(program: &IRProgram) {
    for pkg in &program.packages {
        for (owner, function) in &pkg.functions {
            for block in &function.blocks {
                for inst in &block.instructions {
                    if let IRInstruction::Call { callee, .. } = inst
                        && program.function(callee.mangled()).is_none()
                    {
                        seal_panic(&format!(
                            "function `{owner}` calls `{callee}`, but that function is not \
                             registered in the IRProgram",
                        ));
                    }
                }
            }
        }
    }
}

/// Script counterpart of [`seal_program_calls`]: `IRScript` carries
/// its own `packages` table; both the inline script body and any
/// helper functions inside `packages` may emit calls, and every one
/// of those must resolve to something `script.function()` can find.
fn seal_script_calls(script: &IRScript) {
    for block in &script.blocks {
        for inst in &block.instructions {
            if let IRInstruction::Call { callee, .. } = inst
                && script.function(callee.mangled()).is_none()
            {
                seal_panic(&format!(
                    "script body calls `{callee}`, but that function is not \
                     registered in the IRScript",
                ));
            }
        }
    }
    for pkg in &script.packages {
        for (owner, function) in &pkg.functions {
            for block in &function.blocks {
                for inst in &block.instructions {
                    if let IRInstruction::Call { callee, .. } = inst
                        && script.function(callee.mangled()).is_none()
                    {
                        seal_panic(&format!(
                            "function `{owner}` calls `{callee}`, but that function is not \
                             registered in the IRScript",
                        ));
                    }
                }
            }
        }
    }
}

fn seal_panic(message: &str) -> ! {
    panic!("alpha IR seal violation: {message}");
}
