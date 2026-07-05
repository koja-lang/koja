//! Post-merge pass that inserts an [`IRInstruction::YieldCheck`]
//! cooperative-preemption point at every loop back-edge, before each
//! [`IRTerminator::TailCall`], and at the entry of every call-containing
//! function, so unbounded loops, tail recursion, and deep non-tail
//! recursion alike can't monopolize a worker (or, on WASM, deadlock the
//! only thread).
//!
//! Runs after [`crate::tail_calls::rewrite_tail_calls`] (so `TailCall`
//! terminators already exist) and before `elaborate`. A back-edge is
//! the edge `pred -> succ` where `succ` dominates `pred`; the check is
//! appended to `pred`, running once per iteration just before the
//! branch. `TailCall` blocks have no CFG successor, so the two
//! insertion sites never overlap.
//!
//! Non-tail recursion crosses neither site (each frame is a plain
//! [`IRInstruction::Call`], with no back-edge), so the entry check is
//! what bounds it: a recursion cycle always passes through a function
//! that contains a call, and a check at that function's entry fires once
//! per frame. Leaf and loop-only functions carry no call and are skipped
//! — their loops are already covered, and straight-line work is bounded.

use crate::dominators::{compute_immediate_dominators, dominates, successors};
use crate::function::{
    FunctionKind, IRBasicBlock, IRFunction, IRFunctionParam, IRInstruction, IRTerminator,
};
use crate::package::IRPackage;
use crate::types::ValueId;

/// Insert yield checks into every regular function across `packages`.
pub(crate) fn insert_yield_checks(packages: &mut [IRPackage]) {
    for package in packages {
        for function in package.functions.values_mut() {
            if matches!(function.kind, FunctionKind::Regular) {
                insert_in_body(&mut function.blocks);
                insert_entry_check(function);
            }
        }
    }
}

/// Insert a [`IRInstruction::YieldCheck`] at the entry of a call-containing
/// function, bounding non-tail recursion. A function reachable only through
/// loops or straight-line code carries no call and is left untouched (its
/// loops already yield, its straight-line work is bounded), so leaf functions
/// don't pay for a check they can never need.
///
/// The check lands after the parameter-promotion prologue (the canonical
/// `LocalDecl` -> acquire -> `LocalWrite` run the backends split on), not at
/// absolute index 0, so it never disturbs that prologue's shape.
fn insert_entry_check(function: &mut IRFunction) {
    let contains_call = function.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                IRInstruction::Call { .. } | IRInstruction::CallClosure { .. }
            )
        })
    });
    if !contains_call {
        return;
    }
    let IRFunction { blocks, params, .. } = function;
    let Some(entry) = blocks.first_mut() else {
        return;
    };
    let offset = promotion_prefix_len(params, &entry.instructions);
    entry.instructions.insert(offset, IRInstruction::YieldCheck);
}

/// Length of the entry block's parameter-promotion prologue: per param a
/// `LocalDecl`, an optional acquire (`Clone` here pre-`elaborate`, the `Call`
/// it rewrites to afterward), then a `LocalWrite`. Mirrors the invariant the
/// backends assert so the entry check inserts just past it.
fn promotion_prefix_len(params: &[IRFunctionParam], instructions: &[IRInstruction]) -> usize {
    let mut len = 0;
    for param in params {
        len += 1; // LocalDecl
        if is_param_acquire(instructions.get(len), param.id) {
            len += 1; // Clone (later a clone-glue Call)
        }
        len += 1; // LocalWrite
    }
    len
}

/// Whether `instruction` is the acquire a heap-managed promotion inserts
/// between a param's `LocalDecl` and `LocalWrite`: a `Clone` of the param, or
/// the `Call` `elaborate` rewrites that clone into (the param being its first
/// argument).
fn is_param_acquire(instruction: Option<&IRInstruction>, param: ValueId) -> bool {
    match instruction {
        Some(IRInstruction::Clone { source, .. }) => *source == param,
        Some(IRInstruction::Call { args, .. }) => args.first() == Some(&param),
        _ => false,
    }
}

/// Insert yield checks into a script's inline top-level body (PID 1's
/// entry), which carries source-level loops but never a `TailCall`.
pub(crate) fn insert_yield_checks_in_body(blocks: &mut [IRBasicBlock]) {
    insert_in_body(blocks);
}

fn insert_in_body(blocks: &mut [IRBasicBlock]) {
    let Some(entry) = blocks.first().map(|block| block.id) else {
        return;
    };
    // Back-edges need dominators, which need a branch to exist at all;
    // a function that only returns or tail-calls has none.
    let has_branch = blocks.iter().any(|block| {
        matches!(
            block.terminator,
            IRTerminator::Branch(_) | IRTerminator::CondBranch { .. }
        )
    });
    let immediate_dominators = has_branch.then(|| compute_immediate_dominators(blocks, entry));

    for block in blocks.iter_mut() {
        let is_tail_call = matches!(block.terminator, IRTerminator::TailCall { .. });
        let on_back_edge = immediate_dominators.as_ref().is_some_and(|idoms| {
            successors(&block.terminator)
                .iter()
                .any(|succ| dominates(idoms, entry, *succ, block.id))
        });
        if is_tail_call || on_back_edge {
            block.instructions.push(IRInstruction::YieldCheck);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::function::{BranchTarget, IRBlockId, IRFunction, IRFunctionParam, IRSymbol};
    use crate::local::IRLocalId;
    use crate::types::{IRType, ValueId};
    use std::collections::BTreeMap;

    fn block(id: u32, terminator: IRTerminator) -> IRBasicBlock {
        IRBasicBlock {
            id: IRBlockId(id),
            label: format!("bb{id}"),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator,
        }
    }

    fn branch(to: u32) -> IRTerminator {
        IRTerminator::Branch(BranchTarget::to(IRBlockId(to)))
    }

    fn cond(then_to: u32, else_to: u32) -> IRTerminator {
        IRTerminator::CondBranch {
            cond: ValueId(0),
            then_target: BranchTarget::to(IRBlockId(then_to)),
            else_target: BranchTarget::to(IRBlockId(else_to)),
        }
    }

    fn yield_checks(block: &IRBasicBlock) -> usize {
        block
            .instructions
            .iter()
            .filter(|inst| matches!(inst, IRInstruction::YieldCheck))
            .count()
    }

    #[test]
    fn while_loop_back_edge_gets_a_check() {
        // entry -> header; header -CondBranch-> body, exit;
        // body -Branch-> header (back-edge); exit -> return.
        let mut blocks = vec![
            block(0, branch(1)),
            block(1, cond(2, 3)),
            block(2, branch(1)),
            block(3, IRTerminator::Return { value: None }),
        ];
        insert_in_body(&mut blocks);
        assert_eq!(
            yield_checks(&blocks[2]),
            1,
            "body back-edge must be checked"
        );
        assert_eq!(yield_checks(&blocks[0]), 0);
        assert_eq!(yield_checks(&blocks[1]), 0);
        assert_eq!(yield_checks(&blocks[3]), 0);
    }

    #[test]
    fn single_block_self_loop_gets_a_check() {
        let mut blocks = vec![block(0, branch(0))];
        insert_in_body(&mut blocks);
        assert_eq!(yield_checks(&blocks[0]), 1);
    }

    #[test]
    fn straight_line_function_gets_no_checks() {
        let mut blocks = vec![
            block(0, branch(1)),
            block(1, IRTerminator::Return { value: None }),
        ];
        insert_in_body(&mut blocks);
        assert_eq!(yield_checks(&blocks[0]), 0);
        assert_eq!(yield_checks(&blocks[1]), 0);
    }

    #[test]
    fn forward_branch_is_not_a_back_edge() {
        // A diamond: no edge targets a dominator, so no checks.
        let mut blocks = vec![
            block(0, cond(1, 2)),
            block(1, branch(3)),
            block(2, branch(3)),
            block(3, IRTerminator::Return { value: None }),
        ];
        insert_in_body(&mut blocks);
        assert!(blocks.iter().all(|b| yield_checks(b) == 0));
    }

    #[test]
    fn tail_call_block_gets_a_check() {
        let symbol = IRSymbol::synthetic("Test.loop_forever".to_string());
        let function = IRFunction {
            def_location: None,
            blocks: vec![block(
                0,
                IRTerminator::TailCall {
                    args: Vec::new(),
                    callee: symbol.clone(),
                },
            )],
            kind: FunctionKind::Regular,
            params: Vec::<IRFunctionParam>::new(),
            return_type: IRType::Int64,
            symbol: symbol.clone(),
        };
        let mut functions = BTreeMap::new();
        functions.insert(symbol, function);
        let mut packages = vec![IRPackage {
            constants: BTreeMap::new(),
            enums: BTreeMap::new(),
            functions,
            package: "Test".to_string(),
            structs: BTreeMap::new(),
            unions: BTreeMap::new(),
        }];
        insert_yield_checks(&mut packages);
        let function = packages[0].functions.values().next().unwrap();
        assert_eq!(yield_checks(&function.blocks[0]), 1);
    }

    fn regular_function(blocks: Vec<IRBasicBlock>) -> Vec<IRPackage> {
        regular_function_with_params(Vec::new(), blocks)
    }

    fn regular_function_with_params(
        params: Vec<IRFunctionParam>,
        blocks: Vec<IRBasicBlock>,
    ) -> Vec<IRPackage> {
        let symbol = IRSymbol::synthetic("Test.subject".to_string());
        let function = IRFunction {
            def_location: None,
            blocks,
            kind: FunctionKind::Regular,
            params,
            return_type: IRType::Int64,
            symbol: symbol.clone(),
        };
        let mut functions = BTreeMap::new();
        functions.insert(symbol, function);
        vec![IRPackage {
            constants: BTreeMap::new(),
            enums: BTreeMap::new(),
            functions,
            package: "Test".to_string(),
            structs: BTreeMap::new(),
            unions: BTreeMap::new(),
        }]
    }

    #[test]
    fn call_containing_function_gets_entry_check() {
        let mut entry = block(0, IRTerminator::Return { value: None });
        entry.instructions.push(IRInstruction::Call {
            dest: ValueId(0),
            callee: IRSymbol::synthetic("Test.callee".to_string()),
            args: Vec::new(),
        });
        let mut packages = regular_function(vec![entry]);
        insert_yield_checks(&mut packages);
        let function = packages[0].functions.values().next().unwrap();
        assert_eq!(yield_checks(&function.blocks[0]), 1);
        assert!(
            matches!(
                function.blocks[0].instructions[0],
                IRInstruction::YieldCheck
            ),
            "entry check must lead the block"
        );
    }

    #[test]
    fn leaf_function_gets_no_entry_check() {
        let entry = block(0, IRTerminator::Return { value: None });
        let mut packages = regular_function(vec![entry]);
        insert_yield_checks(&mut packages);
        let function = packages[0].functions.values().next().unwrap();
        assert_eq!(yield_checks(&function.blocks[0]), 0);
    }

    #[test]
    fn entry_check_follows_param_promotion() {
        let param = IRFunctionParam {
            id: ValueId(10),
            local_id: IRLocalId::synthetic_placeholder(),
            ty: IRType::Int64,
        };
        let mut entry = block(0, IRTerminator::Return { value: None });
        entry.instructions = vec![
            IRInstruction::LocalDecl {
                local: param.local_id,
                ty: IRType::Int64,
            },
            IRInstruction::LocalWrite {
                local: param.local_id,
                value: param.id,
            },
            IRInstruction::Call {
                dest: ValueId(0),
                callee: IRSymbol::synthetic("Test.callee".to_string()),
                args: Vec::new(),
            },
        ];
        let mut packages = regular_function_with_params(vec![param], vec![entry]);
        insert_yield_checks(&mut packages);
        let function = packages[0].functions.values().next().unwrap();
        assert_eq!(yield_checks(&function.blocks[0]), 1);
        assert!(
            matches!(
                function.blocks[0].instructions[2],
                IRInstruction::YieldCheck
            ),
            "entry check must follow the LocalDecl/LocalWrite promotion prologue"
        );
    }
}
