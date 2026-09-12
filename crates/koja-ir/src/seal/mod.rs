//! Seal sub-pass: walks the merged [`crate::IRProgram`] /
//! [`crate::IRScript`] and asserts the sealed-IR invariants per the
//! [`COMPILER-NORTHSTAR.md`] contract. Panics on violation, since seal
//! failures indicate compiler bugs in upstream sub-passes, not user
//! errors.
//!
//! [`COMPILER-NORTHSTAR.md`]: ../../design/COMPILER-NORTHSTAR.md
//!
//! [`program`] and [`script`] are the entry points for the two IR
//! shapes. The submodules split by what they check: [`function`]
//! (block shape, dominance of operand definitions, branch targets,
//! locals, tail calls), [`types`] (per-instruction operand and result
//! types), [`structs`], [`enums`], and [`closures`] (decl shape and
//! the instructions that project them). Every `Call` must resolve to
//! a function somewhere in the program or script.

use crate::enum_decl::EnumPayloadInit;
use crate::function::{IRBlockId, IRInstruction, IRTerminator};
use crate::types::{ConstValue, IRType, ValueId};

mod closures;
mod enums;
mod function;
mod program;
mod script;
mod structs;
mod types;

pub(crate) use program::seal_program;
pub(crate) use script::seal_script;

/// Every [`IRType`] variant is admitted. The walk recurses into
/// pointee, element, and member types so a future restriction has a
/// location-aware hook at every edge.
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
        IRType::Indirect(inner) => {
            require_supported_type(inner, &|| format!("{} (Indirect pointee)", location()))
        }
        IRType::List(inner) => {
            require_supported_type(inner, &|| format!("{} (List element)", location()))
        }
        IRType::Map { key, value } => {
            require_supported_type(key, &|| format!("{} (Map key)", location()));
            require_supported_type(value, &|| format!("{} (Map value)", location()));
        }
        IRType::Set(inner) => {
            require_supported_type(inner, &|| format!("{} (Set element)", location()))
        }
        IRType::Function { params, ret, .. } => {
            for (idx, param) in params.iter().enumerate() {
                require_supported_type(param, &|| {
                    format!("{} (Function param[{idx}])", location())
                });
            }
            require_supported_type(ret, &|| format!("{} (Function return)", location()));
        }
        IRType::Tuple(elements) => {
            for (idx, element) in elements.iter().enumerate() {
                require_supported_type(element, &|| {
                    format!("{} (Tuple element[{idx}])", location())
                });
            }
        }
        IRType::Union { members, .. } => {
            for (idx, member) in members.iter().enumerate() {
                require_supported_type(member, &|| format!("{} (Union member[{idx}])", location()));
            }
        }
    }
}

/// Every [`ConstValue`] variant is admitted. The hook exists so a
/// future variant opts in explicitly.
pub(super) fn require_supported_const(value: &ConstValue, location: &dyn Fn() -> String) {
    let _ = (value, location);
}

pub(super) fn instruction_operands(inst: &IRInstruction) -> Vec<ValueId> {
    match inst {
        IRInstruction::BinaryConstruct { segments, .. } => {
            segments.iter().map(|s| s.value()).collect()
        }
        IRInstruction::BinaryMatch { subject, .. } => vec![*subject],
        IRInstruction::BinaryOp { lhs, rhs, .. } => vec![*lhs, *rhs],
        IRInstruction::Call { args, .. } => args.clone(),
        IRInstruction::CallClosure { args, callee, .. } => {
            let mut operands = vec![*callee];
            operands.extend(args.iter().copied());
            operands
        }
        IRInstruction::Clone { source, .. } | IRInstruction::DeepCopy { source, .. } => {
            vec![*source]
        }
        IRInstruction::ClosureEquals { lhs, rhs, .. } | IRInstruction::Concat { lhs, rhs, .. } => {
            vec![*lhs, *rhs]
        }
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
        IRInstruction::FieldSet { base, value, .. } => vec![*base, *value],
        IRInstruction::IndirectPresent { base, .. } => vec![*base],
        // `LoadCapture` reads from the enclosing closure's env, not
        // a `ValueId`, so there is nothing to validate in the per-block walk.
        IRInstruction::LoadCapture { .. } => vec![],
        IRInstruction::LoadCaptureOf { closure, .. } => vec![*closure],
        // `LoadConst` reads from the package constant pool, not a
        // `ValueId`, so it has no operand to validate here. The
        // pool entry is checked against the program-level constants
        // index by `seal_program_loadconst_pool` and
        // `seal_script_loadconst_pool`.
        IRInstruction::LoadConst { .. } => vec![],
        // `ConsumeLocal`, `DropLocal`, `LocalDecl`, and `LocalRead`
        // name a slot, not a `ValueId`, so the per-block defined-set
        // walk has nothing to validate here. The slot is checked
        // against the per-function decl set by `seal_locals`.
        IRInstruction::ConsumeLocal { .. }
        | IRInstruction::DropLocal { .. }
        | IRInstruction::LocalDecl { .. }
        | IRInstruction::LocalRead { .. } => vec![],
        IRInstruction::DropValue { value, .. } => vec![*value],
        IRInstruction::LocalWrite { value, .. } => vec![*value],
        IRInstruction::MakeClosure { captures, .. } => captures.clone(),
        IRInstruction::NumericWiden { value, .. } => vec![*value],
        // `Receive` consumes only the optional `after` timeout
        // value. Arm payloads bind into pre-declared local slots
        // that the surrounding function-level walk validates.
        IRInstruction::Receive { after, .. } => {
            after.as_ref().map(|a| vec![a.timeout]).unwrap_or_default()
        }
        // `Spawn` consumes the config value. The wrapper symbol
        // resolves through the program's function index, validated
        // by `seal_program_calls`.
        IRInstruction::Spawn { config, .. } => vec![*config],
        // `ProcessExit` / `SetPriority` consume their single tag value.
        // The defined-before-use walk validates it like any other operand.
        IRInstruction::ProcessExit { reason } => vec![*reason],
        IRInstruction::SetPriority { tag } => vec![*tag],
        // `YieldCheck` is a bare preemption point: no operands, no dest.
        IRInstruction::YieldCheck => vec![],
        IRInstruction::StructInit { fields, .. } => fields.iter().map(|f| f.value).collect(),
        IRInstruction::TupleGet { base, .. } => vec![*base],
        IRInstruction::TupleInit { elements, .. } => elements.clone(),
        IRInstruction::UnaryOp { operand, .. } => vec![*operand],
        IRInstruction::UnionPayloadGet { value, .. } | IRInstruction::UnionTagGet { value, .. } => {
            vec![*value]
        }
        IRInstruction::UnionWrap { value, .. } => vec![*value],
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
        IRTerminator::TailCall { args, .. } => args.clone(),
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
        IRTerminator::Return { .. } | IRTerminator::TailCall { .. } | IRTerminator::Unreachable => {
            vec![]
        }
    }
}

pub(super) fn seal_panic(message: &str) -> ! {
    panic!("IR seal violation: {message}");
}
