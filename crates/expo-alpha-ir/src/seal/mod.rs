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
//! - [`structs`] — `seal_struct_decls` (per-package decl shape)
//!   plus `seal_struct_ops` (cross-instruction `StructInit` /
//!   `FieldGet` validation, fed by an `IRSymbol -> IRStructDecl`
//!   closure the program / script paths supply).
//! - This module ([`mod.rs`]) — shared helpers used by all
//!   submodules: [`seal_panic`], [`require_supported_type`],
//!   [`require_supported_const`], [`instruction_operands`],
//!   [`terminator_operands`], [`terminator_targets`].
//!
//! Invariants asserted (program path):
//!
//! 1. The entry-point [`crate::IRSymbol`] resolves to a registered
//!    function.
//! 2. Every function in every package keys at its own symbol
//!    (`pkg.functions[sym].symbol == sym`).
//! 3. Per-function body shape matches its [`crate::FunctionKind`]:
//!    `Regular` carries at least one basic block; `Intrinsic`
//!    carries zero (the body is synthesized at emit time by the
//!    backend's `intrinsics/` dispatch).
//! 4. Every basic-block id is unique within its function.
//! 5. Every operand referenced by an instruction or terminator points
//!    at a `ValueId` whose definition dominates the using block — i.e.
//!    the def lives in the using block itself or in some block that
//!    sits on every path from the entry block to it. Function
//!    parameter `ValueId`s seed the entry block's scope, so body
//!    references to params are valid without a distinct
//!    "definition" instruction. Block parameters define their
//!    `dest` on entry to the declaring block, so the dominator-tree
//!    walk picks them up at the right level. Cross-block value flow
//!    via local slots (`LocalDecl` / `LocalWrite` / `LocalRead`)
//!    flows on top of this and is checked separately by
//!    `seal_locals`.
//! 6. Every `IRTerminator::Branch` / `CondBranch` target is a block
//!    that exists in the same function.
//! 7. Every `IRInstruction::Call`'s `callee` symbol resolves to a
//!    function that actually exists somewhere in the `IRProgram` /
//!    `IRScript`.
//! 8. **Transient slice invariant**: every [`ConstValue`] that flows
//!    through the IR is one of `Bool`, `Float64`, `Int64`, `String`,
//!    or `Unit`. The narrower / unsigned / `Float32` width variants
//!    exist in the [`ConstValue`] vocabulary but are forbidden until
//!    literal width inference lands — there's no surface syntax that
//!    materializes them yet. The [`IRType`] vocabulary is broader:
//!    every variant is admitted, since FFI signatures (and any
//!    regular function that propagates an FFI value) legitimately
//!    surface explicit-width primitives (`Int8`..`UInt64`,
//!    `Float32`/`Float64`, `CPtr<T>`).
//! 9. Every struct declaration has dense, declaration-order field
//!    indices (`0..n`), unique field names, and field types in the
//!    transient set. Every `IRInstruction::StructInit` carries
//!    exactly the decl's field count, field-init indices match
//!    declaration positions, and `ty` resolves to a registered
//!    decl. Every `IRInstruction::FieldGet` has a `field_index`
//!    in range and a `field_type` that matches
//!    `IRStructField::ir_type` on the resolved decl.
//!
//! The script path ([`seal_script`]) re-asserts (3)–(9) on the
//! implicit-function shape ([`crate::IRScript::blocks`] +
//! [`crate::IRScript::return_type`]), and re-asserts (7) using
//! [`crate::IRScript::packages`] as the call-target lookup.

use crate::enum_decl::EnumPayloadInit;
use crate::function::{IRBlockId, IRInstruction, IRTerminator};
use crate::types::{ConstValue, IRType, ValueId};

mod enums;
mod function;
mod program;
mod script;
mod structs;

pub(crate) use program::seal_program;
pub(crate) use script::seal_script;

/// Every [`IRType`] variant is admitted. The narrower / explicit-
/// width numeric variants and `CPtr<T>` are reachable through
/// extern-fn signatures (`FunctionKind::Extern` declarations) and
/// through regular function bodies that propagate FFI values; the
/// rest are reachable through ordinary user code. Inner `CPtr`
/// pointees recurse so `CPtr<CPtr<UInt8>>` rejects nothing
/// structurally.
///
/// Kept as a function (not deleted) so the per-edge call sites in
/// [`function::seal_function`] retain their location-aware error
/// surface — useful when seal panics ever loosen back into recoverable
/// diagnostics. See module docstring invariant 8.
pub(super) fn require_supported_type(ty: &IRType, location: &dyn Fn() -> String) {
    match ty {
        IRType::Binary
        | IRType::Bits
        | IRType::Bool
        | IRType::Enum(_)
        | IRType::Float32
        | IRType::Float64
        | IRType::Int8
        | IRType::Int16
        | IRType::Int32
        | IRType::Int64
        | IRType::String
        | IRType::Struct(_)
        | IRType::UInt8
        | IRType::UInt16
        | IRType::UInt32
        | IRType::UInt64
        | IRType::Unit => {}
        IRType::CPtr(inner) => {
            require_supported_type(inner, &|| format!("{} (CPtr pointee)", location()))
        }
        IRType::Function { params, ret } => {
            for (idx, param) in params.iter().enumerate() {
                require_supported_type(param, &|| {
                    format!("{} (Function param[{idx}])", location())
                });
            }
            require_supported_type(ret, &|| format!("{} (Function return)", location()));
        }
    }
}

pub(super) fn require_supported_const(value: &ConstValue, location: &dyn Fn() -> String) {
    match value {
        ConstValue::Binary(_)
        | ConstValue::Bits { .. }
        | ConstValue::Bool(_)
        | ConstValue::Float64(_)
        | ConstValue::Int8(_)
        | ConstValue::Int64(_)
        | ConstValue::String(_)
        | ConstValue::Unit => {}
        other => seal_panic(&format!(
            "{}: ConstValue `{other:?}` is not yet admitted (alpha admits only \
             Binary / Bits / Bool / Float64 / Int8 / Int64 / String / Unit)",
            location(),
        )),
    }
}

pub(super) fn instruction_operands(inst: &IRInstruction) -> Vec<ValueId> {
    match inst {
        IRInstruction::BinaryConstruct { segments, .. } => {
            segments.iter().map(|s| s.value()).collect()
        }
        IRInstruction::BinaryOp { lhs, rhs, .. } => vec![*lhs, *rhs],
        IRInstruction::Call { args, .. } => args.clone(),
        IRInstruction::CallClosure { args, callee, .. } => {
            let mut operands = vec![*callee];
            operands.extend(args.iter().copied());
            operands
        }
        IRInstruction::Concat { lhs, rhs, .. } => vec![*lhs, *rhs],
        IRInstruction::Const { .. } => vec![],
        IRInstruction::EnumConstruct { payload, .. } => match payload {
            EnumPayloadInit::Struct(fields) => fields.iter().map(|f| f.value).collect(),
            EnumPayloadInit::Tuple(values) => values.clone(),
            EnumPayloadInit::Unit => vec![],
        },
        IRInstruction::EnumPayloadFieldGet { value, .. }
        | IRInstruction::EnumTagGet { value, .. } => {
            vec![*value]
        }
        IRInstruction::FieldGet { base, .. } => vec![*base],
        // `LoadConst` reads from the package constant pool, not a
        // `ValueId`, so it has no operand to validate here — the
        // pool entry is checked against the program-level constants
        // index by `seal_loadconst_pool`.
        IRInstruction::LoadConst { .. } => vec![],
        // `DropLocal` and `MoveOutLocal` consume a slot, not a
        // `ValueId`; the slot's existence is checked by
        // `seal_locals_in_function`. `MoveOutLocal` produces a new
        // value (its `dest`); `DropLocal` produces nothing.
        // `LocalDecl` declares the slot; nothing in scope yet to read.
        // `LocalRead` reads the slot named by `local`, not a `ValueId`,
        // so the per-block defined-set walk has nothing to validate
        // here — `local` is checked against the per-function decl set
        // by `seal_locals_in_function`.
        IRInstruction::DropLocal { .. }
        | IRInstruction::LocalDecl { .. }
        | IRInstruction::LocalRead { .. }
        | IRInstruction::MoveOutLocal { .. } => vec![],
        IRInstruction::LocalWrite { value, .. } => vec![*value],
        IRInstruction::MakeClosure { captures, .. } => captures.clone(),
        IRInstruction::StructInit { fields, .. } => fields.iter().map(|f| f.value).collect(),
        IRInstruction::UnaryOp { operand, .. } => vec![*operand],
    }
}

pub(super) fn terminator_operands(term: &IRTerminator) -> Vec<ValueId> {
    match term {
        IRTerminator::Branch(target) => target.args.clone(),
        IRTerminator::CondBranch {
            cond,
            else_target,
            then_target,
        } => {
            let mut operands =
                Vec::with_capacity(1 + then_target.args.len() + else_target.args.len());
            operands.push(*cond);
            operands.extend(then_target.args.iter().copied());
            operands.extend(else_target.args.iter().copied());
            operands
        }
        IRTerminator::Return { value } => value.iter().copied().collect(),
        IRTerminator::Unreachable => vec![],
    }
}

pub(super) fn terminator_targets(term: &IRTerminator) -> Vec<IRBlockId> {
    match term {
        IRTerminator::Branch(target) => vec![target.block],
        IRTerminator::CondBranch {
            then_target,
            else_target,
            ..
        } => vec![then_target.block, else_target.block],
        IRTerminator::Return { .. } | IRTerminator::Unreachable => vec![],
    }
}

pub(super) fn seal_panic(message: &str) -> ! {
    panic!("alpha IR seal violation: {message}");
}
