//! Seal sub-pass: walks the merged [`IRProgram`] and asserts the
//! sealed-IRProgram invariants per the [`COMPILER-NORTHSTAR.md`]
//! contract. Panics on violation — seal failures indicate compiler
//! bugs in upstream sub-passes, not user errors.
//!
//! [`COMPILER-NORTHSTAR.md`]: ../../design/COMPILER-NORTHSTAR.md
//!
//! Invariants asserted:
//!
//! 1. The entry-point identifier resolves to a registered function.
//! 2. Every function in every package keys at its own identifier
//!    (`pkg.functions[id].identifier == id`).
//! 3. Every function has at least one basic block.
//! 4. Within each function: every value reference (instruction operand
//!    or terminator value) points at a `ValueId` defined earlier in
//!    the same function.

use std::collections::BTreeSet;

use expo_ast::identifier::Identifier;

use crate::function::{IRBasicBlock, IRFunction, IRInstruction, IRTerminator};
use crate::types::ValueId;
use crate::{IRProgram, package::IRPackage};

pub(crate) fn seal_program(program: &IRProgram) {
    if program.function(&program.entry_point).is_none() {
        seal_panic(&format!(
            "entry point `{}` not registered in any package",
            program.entry_point
        ));
    }
    for pkg in &program.packages {
        seal_package(pkg);
    }
}

fn seal_package(pkg: &IRPackage) {
    for (id, function) in &pkg.functions {
        if id != &function.identifier {
            seal_panic(&format!(
                "package `{}` keys function at `{id}` but the function's own identifier is `{}`",
                pkg.package, function.identifier,
            ));
        }
        seal_function(function);
    }
}

fn seal_function(function: &IRFunction) {
    if function.blocks.is_empty() {
        seal_panic(&format!(
            "function `{}` has no basic blocks",
            function.identifier,
        ));
    }
    let mut defined: BTreeSet<ValueId> = BTreeSet::new();
    for block in &function.blocks {
        seal_block(block, &function.identifier, &mut defined);
    }
}

fn seal_block(block: &IRBasicBlock, owner: &Identifier, defined: &mut BTreeSet<ValueId>) {
    for inst in &block.instructions {
        for operand in instruction_operands(inst) {
            require_defined(operand, owner, defined);
        }
        if !defined.insert(inst.dest()) {
            seal_panic(&format!(
                "function `{owner}` redefines value `{}`",
                inst.dest(),
            ));
        }
    }
    for operand in terminator_operands(&block.terminator) {
        require_defined(operand, owner, defined);
    }
}

fn instruction_operands(inst: &IRInstruction) -> Vec<ValueId> {
    match inst {
        IRInstruction::BinaryOp { lhs, rhs, .. } => vec![*lhs, *rhs],
        IRInstruction::Const { .. } => vec![],
        IRInstruction::UnaryOp { operand, .. } => vec![*operand],
    }
}

fn terminator_operands(term: &IRTerminator) -> Vec<ValueId> {
    match term {
        IRTerminator::Return { value } => value.iter().copied().collect(),
    }
}

fn require_defined(value: ValueId, owner: &Identifier, defined: &BTreeSet<ValueId>) {
    if !defined.contains(&value) {
        seal_panic(&format!(
            "function `{owner}` references value `{value}` before it is defined",
        ));
    }
}

fn seal_panic(message: &str) -> ! {
    panic!("alpha IR seal violation: {message}");
}
