//! Post-merge tail-call rewrite pass. Walks every regular function
//! across an [`crate::IRProgram`] / [`crate::IRScript`] and rewrites
//! self-recursive call-then-return shapes into [`IRTerminator::TailCall`].
//!
//! The lowering layer never emits `TailCall` directly — the pattern
//! is best detected on the merged IR after monomorphization, when
//! every callee resolves to its final symbol. Backends consume the
//! rewritten IR and turn `TailCall` into in-frame state rebinding
//! plus a jump (LLVM: store args + branch to a per-function loop
//! header; eval: trampoline back through `execute_function` with
//! the new args).
//!
//! Detection shape (per block):
//!
//! ```text
//! ... evaluate args (LocalReads, struct inits, ...)
//! %c = Call { callee: F.symbol, args: [a0, a1, ...] }
//! DropLocal X     // any number of trailing exit drops
//! DropValue Y
//! Return Some(%c)
//! ```
//!
//! Trailing drops are preserved (they release this iteration's owned
//! locals before the back-edge). The `Call` is removed; the `Return`
//! is replaced by `TailCall { callee, args }`.
//!
//! Cross-function tail calls (callee != enclosing symbol) and
//! intermediate non-drop instructions disqualify a candidate.

use crate::function::{
    FunctionKind, IRBasicBlock, IRFunction, IRInstruction, IRSymbol, IRTerminator,
};
use crate::package::IRPackage;
use crate::types::ValueId;

/// Rewrite every self-recursive tail-position call across `packages`
/// into [`IRTerminator::TailCall`]. Idempotent — re-running on an
/// already-rewritten IR is a no-op since `TailCall` blocks no longer
/// match the `Return`-terminated detection pattern.
pub(crate) fn rewrite_tail_calls(packages: &mut [IRPackage]) {
    for pkg in packages {
        for function in pkg.functions.values_mut() {
            if matches!(function.kind, FunctionKind::Regular) {
                rewrite_function(function);
            }
        }
    }
}

/// Whether `function` carries any [`IRTerminator::TailCall`]
/// terminator after [`rewrite_tail_calls`] has run. Backends consult
/// this to decide whether to install a per-function loop header
/// without re-running the rewrite pattern.
pub fn function_has_tail_call(function: &IRFunction) -> bool {
    function
        .blocks
        .iter()
        .any(|block| matches!(block.terminator, IRTerminator::TailCall { .. }))
}

fn rewrite_function(function: &mut IRFunction) {
    let symbol = function.symbol.clone();
    for block in &mut function.blocks {
        if let Some(plan) = match_tail_call(block, &symbol) {
            apply_plan(block, plan, &symbol);
        }
    }
}

/// Detection-time payload: which instruction index holds the
/// candidate `Call` and what its `args` are. The block walk applies
/// this in [`apply_plan`] after the immutable scan completes.
struct RewritePlan {
    args: Vec<ValueId>,
    call_index: usize,
}

fn match_tail_call(block: &IRBasicBlock, enclosing: &IRSymbol) -> Option<RewritePlan> {
    let returned = match &block.terminator {
        IRTerminator::Return { value: Some(id) } => Some(*id),
        IRTerminator::Return { value: None } => None,
        _ => return None,
    };
    for (index, inst) in block.instructions.iter().enumerate().rev() {
        match inst {
            IRInstruction::DropLocal { .. } | IRInstruction::DropValue { .. } => continue,
            IRInstruction::Call { dest, callee, args } => {
                if callee != enclosing {
                    return None;
                }
                if let Some(returned_id) = returned
                    && returned_id != *dest
                {
                    return None;
                }
                return Some(RewritePlan {
                    args: args.clone(),
                    call_index: index,
                });
            }
            _ => return None,
        }
    }
    None
}

fn apply_plan(block: &mut IRBasicBlock, plan: RewritePlan, enclosing: &IRSymbol) {
    block.instructions.remove(plan.call_index);
    block.terminator = IRTerminator::TailCall {
        callee: enclosing.clone(),
        args: plan.args,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::function::{IRBasicBlock, IRBlockId, IRFunctionParam};
    use crate::local::IRLocalId;
    use crate::types::{IRType, ValueId};
    use koja_ast::identifier::LocalId;
    use std::collections::BTreeMap;

    fn local(n: u32) -> IRLocalId {
        IRLocalId::from_local_id(LocalId::new(n))
    }

    /// Build a one-block self-recursive function whose entry block
    /// matches the canonical tail-call shape: param promotion in the
    /// entry, then `Call self(arg)`, then `Return Some(call_dest)`.
    /// The returned function is the rewrite-pass input; assertions
    /// inspect the post-rewrite shape.
    fn build_self_call_function() -> IRFunction {
        let symbol = IRSymbol::synthetic("Test.loop_forever".to_string());
        let param_id = ValueId(0);
        let param_local = local(0);
        let read_dest = ValueId(1);
        let call_dest = ValueId(2);
        IRFunction {
            blocks: vec![IRBasicBlock {
                id: IRBlockId(0),
                label: "entry".to_string(),
                params: Vec::new(),
                instructions: vec![
                    IRInstruction::LocalDecl {
                        local: param_local,
                        ty: IRType::Int64,
                    },
                    IRInstruction::LocalWrite {
                        local: param_local,
                        value: param_id,
                    },
                    IRInstruction::LocalRead {
                        dest: read_dest,
                        local: param_local,
                        ty: IRType::Int64,
                    },
                    IRInstruction::Call {
                        dest: call_dest,
                        callee: symbol.clone(),
                        args: vec![read_dest],
                    },
                ],
                terminator: IRTerminator::Return {
                    value: Some(call_dest),
                },
            }],
            kind: FunctionKind::Regular,
            params: vec![IRFunctionParam {
                id: param_id,
                local_id: param_local,
                ty: IRType::Int64,
            }],
            return_type: IRType::Int64,
            symbol,
        }
    }

    fn package_with(function: IRFunction) -> IRPackage {
        let mut functions = BTreeMap::new();
        functions.insert(function.symbol.clone(), function);
        IRPackage {
            constants: BTreeMap::new(),
            enums: BTreeMap::new(),
            functions,
            package: "Test".to_string(),
            structs: BTreeMap::new(),
            unions: BTreeMap::new(),
        }
    }

    #[test]
    fn self_recursive_call_then_return_rewrites_to_tail_call() {
        let function = build_self_call_function();
        let symbol = function.symbol.clone();
        let mut packages = vec![package_with(function)];
        rewrite_tail_calls(&mut packages);
        let function = packages[0].functions.get(symbol.mangled()).unwrap();
        let block = &function.blocks[0];
        let IRTerminator::TailCall { callee, args } = &block.terminator else {
            panic!("expected TailCall terminator, got {:?}", block.terminator);
        };
        assert_eq!(callee, &symbol);
        assert_eq!(args, &vec![ValueId(1)]);
        assert!(
            !block
                .instructions
                .iter()
                .any(|inst| matches!(inst, IRInstruction::Call { .. })),
            "Call instruction must be removed; got {:?}",
            block.instructions,
        );
    }

    #[test]
    fn cross_function_call_does_not_rewrite() {
        let mut function = build_self_call_function();
        let other = IRSymbol::synthetic("Test.other".to_string());
        for inst in &mut function.blocks[0].instructions {
            if let IRInstruction::Call { callee, .. } = inst {
                *callee = other;
                break;
            }
        }
        let symbol = function.symbol.clone();
        let mut packages = vec![package_with(function)];
        rewrite_tail_calls(&mut packages);
        let function = packages[0].functions.get(symbol.mangled()).unwrap();
        assert!(matches!(
            function.blocks[0].terminator,
            IRTerminator::Return { .. }
        ));
    }

    #[test]
    fn return_value_mismatch_does_not_rewrite() {
        let mut function = build_self_call_function();
        let stray = ValueId(99);
        function.blocks[0].terminator = IRTerminator::Return { value: Some(stray) };
        let symbol = function.symbol.clone();
        let mut packages = vec![package_with(function)];
        rewrite_tail_calls(&mut packages);
        let function = packages[0].functions.get(symbol.mangled()).unwrap();
        assert!(matches!(
            function.blocks[0].terminator,
            IRTerminator::Return {
                value: Some(ValueId(99))
            }
        ));
    }

    #[test]
    fn trailing_drops_between_call_and_return_preserved() {
        let mut function = build_self_call_function();
        function.blocks[0]
            .instructions
            .push(IRInstruction::DropLocal {
                local: local(0),
                ty: IRType::Int64,
            });
        let symbol = function.symbol.clone();
        let mut packages = vec![package_with(function)];
        rewrite_tail_calls(&mut packages);
        let function = packages[0].functions.get(symbol.mangled()).unwrap();
        assert!(matches!(
            function.blocks[0].terminator,
            IRTerminator::TailCall { .. }
        ));
        assert!(
            function.blocks[0]
                .instructions
                .iter()
                .any(|inst| matches!(inst, IRInstruction::DropLocal { .. })),
            "DropLocal must be preserved; got {:?}",
            function.blocks[0].instructions,
        );
    }

    #[test]
    fn idempotent() {
        let function = build_self_call_function();
        let symbol = function.symbol.clone();
        let mut packages = vec![package_with(function)];
        rewrite_tail_calls(&mut packages);
        let before = packages[0].functions.get(symbol.mangled()).unwrap().clone();
        rewrite_tail_calls(&mut packages);
        let after = packages[0].functions.get(symbol.mangled()).unwrap().clone();
        assert_eq!(before.blocks[0].instructions, after.blocks[0].instructions);
        assert_eq!(before.blocks[0].terminator, after.blocks[0].terminator);
    }
}
