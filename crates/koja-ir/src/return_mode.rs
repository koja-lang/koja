//! Return-mode inference: classify every function's result as
//! [`ReturnMode::Owned`] (fresh heap the call site may drop) or
//! [`ReturnMode::Borrowed`] (a view that aliases an input or a
//! static, which must never be dropped). The load-bearing
//! prerequisite from `design/OWNERSHIP-DROP.md` §Ownership — nothing
//! downstream may stamp a call result `Owned` until this runs.
//!
//! Runs as a whole-program pass over the post-monomorphization,
//! post-tail-call IR (so `@intrinsic` symbols, generic specializations,
//! and self-recursive [`IRTerminator::TailCall`]s are all present).
//! It only *computes and stores* the mode — drop insertion that
//! consumes it lands in a later phase, so the tree stays green.
//!
//! Two sources, one memoized DFS over the IR call graph (callees are
//! explicit on [`IRInstruction::Call`] / [`IRTerminator::TailCall`]):
//!
//! - `@intrinsic` bodies are empty, so their mode comes from the
//!   hand-authored [`IRIntrinsicId::return_mode`] catalog.
//! - User bodies are inferred: the function is `Owned` iff *every*
//!   value it returns is owned, where a returned call's ownership is
//!   its callee's mode.
//!
//! Bias: unresolved callees and cycles (recursion / loop-carried
//! values) resolve to `Borrowed`, so the pass can only ever
//! under-report ownership — never free something it shouldn't
//! (leak-not-double-free).

use std::collections::HashMap;

use koja_ast::ast::ReturnMode;

use crate::enum_decl::EnumPayloadInit;
use crate::function::{
    BranchTarget, FunctionKind, IRFunction, IRInstruction, IRSymbol, IRTerminator,
};
use crate::local::IRLocalId;
use crate::ownership::Ownership;
use crate::package::IRPackage;
use crate::types::ValueId;

/// Compute and write [`IRFunction::return_mode`] for every function
/// across `packages`. Functions whose symbol isn't reachable in the
/// index keep their construction-time default.
pub fn infer_return_modes(packages: &mut [IRPackage]) {
    let index: HashMap<&IRSymbol, &IRFunction> = packages
        .iter()
        .flat_map(|pkg| pkg.functions.iter())
        .collect();

    let mut memo: HashMap<IRSymbol, ReturnMode> = HashMap::new();
    for symbol in index.keys() {
        function_mode(symbol, &index, &mut memo, &mut Vec::new());
    }
    drop(index);

    for package in packages.iter_mut() {
        for (symbol, function) in package.functions.iter_mut() {
            if let Some(mode) = memo.get(symbol) {
                function.return_mode = *mode;
            }
        }
    }
}

type Index<'a> = HashMap<&'a IRSymbol, &'a IRFunction>;
type Memo = HashMap<IRSymbol, ReturnMode>;

/// The per-edge branch payloads of `terminator` (empty for the
/// non-branching exits). Local mirror of the private `successors`
/// helpers elsewhere — those return block ids; merge-param tracing
/// needs the [`BranchTarget::args`] too.
fn branch_targets(terminator: &IRTerminator) -> Vec<&BranchTarget> {
    match terminator {
        IRTerminator::Branch(target) => vec![target],
        IRTerminator::CondBranch {
            else_target,
            then_target,
            ..
        } => vec![else_target, then_target],
        IRTerminator::Return { .. } | IRTerminator::TailCall { .. } | IRTerminator::Unreachable => {
            Vec::new()
        }
    }
}

/// `Owned` iff `lhs` and `rhs` are both owned. The fold identity is
/// `Owned`, matching "a function with no value-bearing return is
/// vacuously owned" (its mode is moot — the result is `Unit`).
fn both(lhs: ReturnMode, rhs: ReturnMode) -> ReturnMode {
    match (lhs, rhs) {
        (ReturnMode::Owned, ReturnMode::Owned) => ReturnMode::Owned,
        _ => ReturnMode::Borrowed,
    }
}

/// Memoized mode of the function named `symbol`. `visiting` is the
/// active DFS stack; a back-edge into it (recursion / mutual
/// recursion) short-circuits to `Borrowed`.
fn function_mode(
    symbol: &IRSymbol,
    index: &Index,
    memo: &mut Memo,
    visiting: &mut Vec<IRSymbol>,
) -> ReturnMode {
    if let Some(&mode) = memo.get(symbol) {
        return mode;
    }
    if visiting.iter().any(|active| active == symbol) {
        return ReturnMode::Borrowed;
    }
    let Some(function) = index.get(symbol).copied() else {
        return ReturnMode::Borrowed;
    };
    let mode = match &function.kind {
        FunctionKind::Intrinsic(id) => id.return_mode(),
        FunctionKind::Closure { .. } | FunctionKind::Regular => {
            visiting.push(symbol.clone());
            let body = body_mode(function, index, memo, visiting);
            visiting.pop();
            body
        }
        // No Koja body to inspect (FFI / spawn thunks); stay leak-safe.
        FunctionKind::Extern(_)
        | FunctionKind::ProcessEntryWrapper { .. }
        | FunctionKind::SpawnWrapper { .. } => ReturnMode::Borrowed,
    };
    memo.insert(symbol.clone(), mode);
    mode
}

/// `Owned` iff every value-bearing exit of `function` is owned.
/// Exits are `Return { value: Some(_) }` and self/cross tail calls
/// (whose result is the callee's).
fn body_mode(
    function: &IRFunction,
    index: &Index,
    memo: &mut Memo,
    visiting: &mut Vec<IRSymbol>,
) -> ReturnMode {
    let view = BodyView::new(function);
    let mut mode = ReturnMode::Owned;
    for block in &function.blocks {
        let exit = match &block.terminator {
            IRTerminator::Return { value: Some(value) } => {
                view.value_mode(*value, index, memo, visiting, &mut Vec::new())
            }
            IRTerminator::TailCall { callee, .. } => function_mode(callee, index, memo, visiting),
            IRTerminator::Branch(_)
            | IRTerminator::CondBranch { .. }
            | IRTerminator::Return { value: None }
            | IRTerminator::Unreachable => continue,
        };
        mode = both(mode, exit);
        if mode == ReturnMode::Borrowed {
            break;
        }
    }
    mode
}

/// Per-function lookup tables for value classification: which
/// instruction defines each `ValueId`, and which block-param each
/// `ValueId` is (so phi-style merge values can be traced back to
/// their incoming edges).
struct BodyView<'a> {
    function: &'a IRFunction,
    producers: HashMap<ValueId, &'a IRInstruction>,
    /// `dest -> (block index in `function.blocks`, param position)`.
    block_params: HashMap<ValueId, (usize, usize)>,
}

impl<'a> BodyView<'a> {
    fn new(function: &'a IRFunction) -> Self {
        let mut producers = HashMap::new();
        let mut block_params = HashMap::new();
        for (block_index, block) in function.blocks.iter().enumerate() {
            for (position, param) in block.params.iter().enumerate() {
                block_params.insert(param.dest, (block_index, position));
            }
            for instruction in &block.instructions {
                if let Some(dest) = instruction.dest() {
                    producers.insert(dest, instruction);
                }
            }
        }
        Self {
            function,
            producers,
            block_params,
        }
    }

    /// Ownership of `value`. `seen` guards loop-carried cycles in the
    /// value graph (a merge param fed by its own block); a re-entry
    /// resolves to `Borrowed`.
    fn value_mode(
        &self,
        value: ValueId,
        index: &Index,
        memo: &mut Memo,
        visiting: &mut Vec<IRSymbol>,
        seen: &mut Vec<ValueId>,
    ) -> ReturnMode {
        if seen.contains(&value) {
            return ReturnMode::Borrowed;
        }
        seen.push(value);
        let mode = self.value_mode_uncached(value, index, memo, visiting, seen);
        seen.pop();
        mode
    }

    fn value_mode_uncached(
        &self,
        value: ValueId,
        index: &Index,
        memo: &mut Memo,
        visiting: &mut Vec<IRSymbol>,
        seen: &mut Vec<ValueId>,
    ) -> ReturnMode {
        let Some(instruction) = self.producers.get(&value).copied() else {
            return self.merge_param_mode(value, index, memo, visiting, seen);
        };
        match instruction {
            // Freshly-allocated heap or a moved-through closure value.
            IRInstruction::BinaryConstruct { .. }
            | IRInstruction::BinaryMatch { .. }
            | IRInstruction::Concat { .. }
            | IRInstruction::MakeClosure { .. }
            | IRInstruction::Receive { .. } => ReturnMode::Owned,
            // The call site's ownership is the callee's return mode.
            IRInstruction::Call { callee, .. } => function_mode(callee, index, memo, visiting),
            // An aggregate is owned only when every payload is owned —
            // wrapping a borrowed field/element doesn't mint ownership.
            IRInstruction::EnumConstruct { payload, .. } => {
                self.payload_mode(payload, index, memo, visiting, seen)
            }
            IRInstruction::StructInit { fields, .. } => {
                fields.iter().fold(ReturnMode::Owned, |mode, field| {
                    both(
                        mode,
                        self.value_mode(field.value, index, memo, visiting, seen),
                    )
                })
            }
            IRInstruction::UnionWrap { value, .. } => {
                self.value_mode(*value, index, memo, visiting, seen)
            }
            // Slot reads inherit the slot's tracked ownership.
            IRInstruction::LocalRead { local, .. } | IRInstruction::MoveOutLocal { local, .. } => {
                self.local_mode(*local)
            }
            // Views, statics, scalars, and indirect/unknown producers:
            // never owned by the call site.
            IRInstruction::BinaryOp { .. }
            | IRInstruction::CallClosure { .. }
            | IRInstruction::Const { .. }
            | IRInstruction::EnumPayloadFieldGet { .. }
            | IRInstruction::EnumTagGet { .. }
            | IRInstruction::FieldGet { .. }
            | IRInstruction::FieldSet { .. }
            | IRInstruction::LoadCapture { .. }
            | IRInstruction::LoadConst { .. }
            | IRInstruction::Spawn { .. }
            | IRInstruction::UnaryOp { .. }
            | IRInstruction::UnionPayloadGet { .. }
            | IRInstruction::UnionTagGet { .. } => ReturnMode::Borrowed,
            // Side-effect-only; never define a returnable value.
            IRInstruction::DropLocal { .. }
            | IRInstruction::DropValue { .. }
            | IRInstruction::LocalDecl { .. }
            | IRInstruction::LocalWrite { .. } => ReturnMode::Borrowed,
        }
    }

    /// `Owned` iff every payload element is owned.
    fn payload_mode(
        &self,
        payload: &EnumPayloadInit,
        index: &Index,
        memo: &mut Memo,
        visiting: &mut Vec<IRSymbol>,
        seen: &mut Vec<ValueId>,
    ) -> ReturnMode {
        match payload {
            EnumPayloadInit::Unit => ReturnMode::Owned,
            EnumPayloadInit::Tuple(values) => {
                values.iter().fold(ReturnMode::Owned, |mode, value| {
                    both(mode, self.value_mode(*value, index, memo, visiting, seen))
                })
            }
            EnumPayloadInit::Struct(fields) => {
                fields.iter().fold(ReturnMode::Owned, |mode, field| {
                    both(
                        mode,
                        self.value_mode(field.value, index, memo, visiting, seen),
                    )
                })
            }
        }
    }

    /// `Owned` iff every write into the slot stamped
    /// [`Ownership::Owned`]. The stamp is lowering's own
    /// ownership_for_expr / ownership_for_param verdict (the single
    /// source of truth) — `move`-param promotion and fresh-heap RHS
    /// stamp `Owned`; borrows, statics, and call results (not yet
    /// widened) stamp `Unowned`. Trusting it keeps the pass aligned
    /// with current drop behaviour and biases conservative.
    fn local_mode(&self, local: IRLocalId) -> ReturnMode {
        let mut mode = ReturnMode::Owned;
        let mut wrote = false;
        for block in &self.function.blocks {
            for instruction in &block.instructions {
                let IRInstruction::LocalWrite {
                    local: written,
                    ownership,
                    ..
                } = instruction
                else {
                    continue;
                };
                if *written != local {
                    continue;
                }
                wrote = true;
                let stored = match ownership {
                    Ownership::Owned => ReturnMode::Owned,
                    Ownership::Unowned => ReturnMode::Borrowed,
                };
                mode = both(mode, stored);
            }
        }
        if wrote { mode } else { ReturnMode::Borrowed }
    }

    /// Trace a merge-block parameter back to the values its
    /// predecessors pass along each incoming edge: `Owned` iff every
    /// arm is owned. Models the `if` / `cond` / `match` join rule.
    fn merge_param_mode(
        &self,
        value: ValueId,
        index: &Index,
        memo: &mut Memo,
        visiting: &mut Vec<IRSymbol>,
        seen: &mut Vec<ValueId>,
    ) -> ReturnMode {
        let Some(&(block_index, position)) = self.block_params.get(&value) else {
            return ReturnMode::Borrowed;
        };
        let target = self.function.blocks[block_index].id;
        let mut mode = ReturnMode::Owned;
        let mut found_edge = false;
        for block in &self.function.blocks {
            for incoming in branch_targets(&block.terminator) {
                if incoming.block != target {
                    continue;
                }
                let Some(arg) = incoming.args.get(position) else {
                    continue;
                };
                found_edge = true;
                mode = both(mode, self.value_mode(*arg, index, memo, visiting, seen));
            }
        }
        if found_edge {
            mode
        } else {
            ReturnMode::Borrowed
        }
    }
}
