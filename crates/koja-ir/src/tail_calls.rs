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
    FunctionKind, IRBasicBlock, IRBlockId, IRFunction, IRInstruction, IRSymbol, IRTerminator,
};
use crate::package::IRPackage;
use crate::types::{IRType, ValueId};

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
    // The forwarder collapse exists solely to expose a self-recursive
    // call buried behind a control-flow merge, so only run it on
    // self-recursive functions. This keeps unrelated merges intact —
    // notably the single-`Return` shape the `main` wrapper depends on
    // (a non-recursive function with a value-producing `if`/`match`
    // tail would otherwise gain multiple return blocks).
    if contains_self_call(function) {
        collapse_return_forwarders(function);
    }
    let symbol = function.symbol.clone();
    let param_types: Vec<IRType> = function.params.iter().map(|p| p.ty.clone()).collect();
    let mut next_value = next_value_id(function);
    for block in &mut function.blocks {
        if let Some(plan) = match_tail_call(block, &symbol) {
            apply_plan(block, plan, &symbol, &param_types, &mut next_value);
        }
    }
}

/// Whether `function` calls itself directly — the prerequisite for
/// any self-tail-call, and the gate on the return-forwarder collapse.
fn contains_self_call(function: &IRFunction) -> bool {
    let symbol = &function.symbol;
    function.blocks.iter().any(|block| {
        block
            .instructions
            .iter()
            .any(|inst| matches!(inst, IRInstruction::Call { callee, .. } if callee == symbol))
    })
}

/// Expose self-tail-calls hidden behind control-flow merges by
/// collapsing **return-forwarding** blocks until none remain.
///
/// A self-recursive call that is the *value* of an `if` / `match` /
/// `receive` reaches the function's `Return` indirectly: the arm
/// branches into a merge block carrying the call result as a
/// [`crate::function::BlockParam`], and that merge block returns the
/// param. The block holding the `Call` therefore ends in a `Branch`,
/// not a `Return`, so [`match_tail_call`] can't see it and the call
/// stays a real frame-growing recursion — fatal for the long-running
/// `receive ... after -> self.loop()` actor idiom (unbounded stack).
///
/// A return-forwarder is a block of the shape
///
/// ```text
/// M(%p):
///   DropLocal a      // zero or more function-exit drops, nothing else
///   DropLocal b
///   Return %p        // returns its sole block param verbatim
/// ```
///
/// Each predecessor reaching `M` by an unconditional `Branch(M, [x])`
/// is rewritten to run `M`'s exit drops in place and `Return x`; `M`
/// then has no predecessors and is removed. Running to a fixpoint
/// peels nested merges outermost-first (a `receive` merge collapses
/// into the `match` merge feeding it, which collapses into that
/// match's arms), surfacing a self-call buried arbitrarily deep.
///
/// Collapse is skipped for a merge whose incoming edges aren't all
/// single-arg unconditional branches (e.g. a no-`else` `if` whose
/// `CondBranch` targets the merge directly) so a partially-rewritten
/// merge never strands a block param with missing predecessors.
fn collapse_return_forwarders(function: &mut IRFunction) {
    while collapse_one_forwarder(function) {}
}

/// Collapse the first collapsible return-forwarder found, returning
/// `true` if one was rewritten. `false` signals the fixpoint.
fn collapse_one_forwarder(function: &mut IRFunction) -> bool {
    // The entry block (index 0) is never a merge; start the scan past
    // it so a degenerate single-block forwarder can't be considered.
    let plan = (1..function.blocks.len()).find_map(|index| {
        let block = &function.blocks[index];
        return_forwarder_param(block)?;
        let edges = collapsible_edges(function, block.id)?;
        Some((block.id, block.instructions.clone(), edges))
    });
    let Some((target, exit_drops, edges)) = plan else {
        return false;
    };
    for (predecessor, arg) in edges {
        let block = &mut function.blocks[predecessor];
        block.instructions.extend(exit_drops.iter().cloned());
        block.terminator = IRTerminator::Return { value: Some(arg) };
    }
    function.blocks.retain(|block| block.id != target);
    true
}

/// The sole block param of `block` when it is a return-forwarder:
/// exactly one param, every instruction a function-exit drop, and a
/// terminator that returns that param verbatim. A `DropValue` of the
/// returned param itself disqualifies the block (it would double-free
/// the value the predecessor is about to return).
fn return_forwarder_param(block: &IRBasicBlock) -> Option<ValueId> {
    let [param] = block.params.as_slice() else {
        return None;
    };
    let IRTerminator::Return {
        value: Some(returned),
    } = &block.terminator
    else {
        return None;
    };
    if *returned != param.dest {
        return None;
    }
    let all_exit_drops = block.instructions.iter().all(|inst| match inst {
        IRInstruction::DropLocal { .. } => true,
        IRInstruction::DropValue { value, .. } => *value != param.dest,
        _ => false,
    });
    all_exit_drops.then_some(param.dest)
}

/// Every predecessor edge into `target` as `(block index, branch arg)`
/// pairs — but only when *all* references to `target` are single-arg
/// unconditional branches. Returns `None` if any edge is a
/// `CondBranch` into `target` (or a `Branch` with the wrong arg
/// count), leaving the merge intact; `None` too when nothing branches
/// to `target`.
fn collapsible_edges(function: &IRFunction, target: IRBlockId) -> Option<Vec<(usize, ValueId)>> {
    let mut edges = Vec::new();
    for (index, block) in function.blocks.iter().enumerate() {
        match &block.terminator {
            IRTerminator::Branch(branch) if branch.block == target => {
                let [arg] = branch.args.as_slice() else {
                    return None;
                };
                edges.push((index, *arg));
            }
            IRTerminator::CondBranch {
                then_target,
                else_target,
                ..
            } if then_target.block == target || else_target.block == target => return None,
            _ => {}
        }
    }
    (!edges.is_empty()).then_some(edges)
}

/// The first unused [`ValueId`] for `function` — one past the maximum
/// id defined by any parameter, block parameter, or instruction. The
/// rewrite mints back-edge arg-clone destinations from here.
fn next_value_id(function: &IRFunction) -> u32 {
    let mut max = 0;
    for param in &function.params {
        max = max.max(param.id.0);
    }
    for block in &function.blocks {
        for block_param in &block.params {
            max = max.max(block_param.dest.0);
        }
        for instruction in &block.instructions {
            if let Some(dest) = instruction.dest() {
                max = max.max(dest.0);
            }
        }
    }
    max + 1
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

fn apply_plan(
    block: &mut IRBasicBlock,
    plan: RewritePlan,
    enclosing: &IRSymbol,
    param_types: &[IRType],
    next_value: &mut u32,
) {
    block.instructions.remove(plan.call_index);

    // Acquire each heap-managed arg into a fresh owned value *where the
    // `Call` sat* — i.e. before the trailing exit drops. The back-edge
    // then stores the clone, so rebinding a param slot never reads
    // through storage the drop just released. Without this, a
    // self-tail-call passing a heap slot (`f(list, ...)`) would drop
    // the slot's allocation and then store the freed pointer back. The
    // emitted `Clone` is an inline `rc++` for a heap leaf; for a
    // composite the later [`crate::elaborate`] pass rewrites it into a
    // `clone_T` call (or a register copy for an all-`Copy` aggregate).
    let mut args = plan.args;
    let mut clones = Vec::new();
    for (arg, ty) in args.iter_mut().zip(param_types) {
        if ty.is_heap_managed() {
            let dest = ValueId(*next_value);
            *next_value += 1;
            clones.push(IRInstruction::Clone {
                dest,
                source: *arg,
                ty: ty.clone(),
            });
            *arg = dest;
        }
    }
    for (offset, clone) in clones.into_iter().enumerate() {
        block.instructions.insert(plan.call_index + offset, clone);
    }

    block.terminator = IRTerminator::TailCall {
        callee: enclosing.clone(),
        args,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::function::{BlockParam, BranchTarget, IRBasicBlock, IRBlockId, IRFunctionParam};
    use crate::local::IRLocalId;
    use crate::types::{ConstValue, IRType, ValueId};
    use koja_ast::identifier::LocalId;
    use std::collections::BTreeMap;

    fn local(n: u32) -> IRLocalId {
        IRLocalId::from_local_id(LocalId::new(n))
    }

    /// Build a one-block self-recursive function whose entry block
    /// matches the canonical tail-call shape: param promotion in the
    /// entry, then `Call self(arg)`, then `Return Some(call_dest)`.
    /// The single param is typed `param_ty`. The returned function is
    /// the rewrite-pass input; assertions inspect the post-rewrite
    /// shape.
    fn build_self_call_with_param(param_ty: IRType) -> IRFunction {
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
                        ty: param_ty.clone(),
                    },
                    IRInstruction::LocalWrite {
                        local: param_local,
                        value: param_id,
                    },
                    IRInstruction::LocalRead {
                        dest: read_dest,
                        local: param_local,
                        ty: param_ty.clone(),
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
                ty: param_ty.clone(),
            }],
            return_type: param_ty,
            symbol,
        }
    }

    /// The canonical Int64-param shape most tests build from — a scalar
    /// param needs no back-edge acquire.
    fn build_self_call_function() -> IRFunction {
        build_self_call_with_param(IRType::Int64)
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

    /// The destination of the `Clone` inserted for the single arg, if
    /// any — the rewrite acquires a heap-managed arg before the
    /// back-edge so the trailing slot drop can't release storage the
    /// new args still reference.
    fn rewritten_arg_clone_dest(function: &IRFunction) -> Option<ValueId> {
        let block = &function.blocks[0];
        let IRTerminator::TailCall { args, .. } = &block.terminator else {
            panic!("expected TailCall terminator, got {:?}", block.terminator);
        };
        let arg = *args.first().expect("tail call carries one arg");
        block.instructions.iter().find_map(|inst| match inst {
            IRInstruction::Clone { dest, .. } if *dest == arg => Some(*dest),
            _ => None,
        })
    }

    #[test]
    fn scalar_arg_is_not_cloned_before_back_edge() {
        let function = build_self_call_with_param(IRType::Int64);
        let symbol = function.symbol.clone();
        let mut packages = vec![package_with(function)];
        rewrite_tail_calls(&mut packages);
        let function = packages[0].functions.get(symbol.mangled()).unwrap();
        assert!(
            rewritten_arg_clone_dest(function).is_none(),
            "a scalar arg needs no acquire: {:?}",
            function.blocks[0].instructions,
        );
    }

    #[test]
    fn heap_leaf_arg_is_acquired_before_back_edge() {
        let function = build_self_call_with_param(IRType::String);
        let symbol = function.symbol.clone();
        let mut packages = vec![package_with(function)];
        rewrite_tail_calls(&mut packages);
        let function = packages[0].functions.get(symbol.mangled()).unwrap();
        assert!(
            rewritten_arg_clone_dest(function).is_some(),
            "a heap-leaf arg must be acquired: {:?}",
            function.blocks[0].instructions,
        );
    }

    #[test]
    fn composite_arg_is_acquired_before_back_edge() {
        let function = build_self_call_with_param(IRType::List(Box::new(IRType::Int64)));
        let symbol = function.symbol.clone();
        let mut packages = vec![package_with(function)];
        rewrite_tail_calls(&mut packages);
        let function = packages[0].functions.get(symbol.mangled()).unwrap();
        // The composite `Clone` is elaborated into a `clone_T` call
        // downstream; here it must be present so the back-edge rebinds
        // an independent value rather than a freed buffer.
        assert!(
            rewritten_arg_clone_dest(function).is_some(),
            "a composite arg must be acquired: {:?}",
            function.blocks[0].instructions,
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

    /// Build the canonical `if`-wrapped self-call: the recursive call
    /// is the value of the `else` arm and reaches `Return` through a
    /// merge-block param, not a direct `Return`. Shape:
    ///
    /// ```text
    /// entry:    CondBranch(cond, then=bb1, else=bb2)
    /// bb1:      Branch merge([const 0])
    /// bb2:      %call = Call self([param read]); Branch merge([%call])
    /// merge(%p): Return %p
    /// ```
    fn build_wrapped_self_call(param_ty: IRType) -> IRFunction {
        let symbol = IRSymbol::synthetic("Test.loop_wrapped".to_string());
        let param_id = ValueId(0);
        let param_local = local(0);
        let cond = ValueId(1);
        let zero = ValueId(2);
        let read_dest = ValueId(3);
        let call_dest = ValueId(4);
        let merge_param = ValueId(5);
        IRFunction {
            blocks: vec![
                IRBasicBlock {
                    id: IRBlockId(0),
                    label: "entry".to_string(),
                    params: Vec::new(),
                    instructions: vec![
                        IRInstruction::LocalDecl {
                            local: param_local,
                            ty: param_ty.clone(),
                        },
                        IRInstruction::LocalWrite {
                            local: param_local,
                            value: param_id,
                        },
                        IRInstruction::Const {
                            dest: cond,
                            value: ConstValue::Bool(true),
                        },
                    ],
                    terminator: IRTerminator::CondBranch {
                        cond,
                        else_target: BranchTarget::to(IRBlockId(2)),
                        then_target: BranchTarget::to(IRBlockId(1)),
                    },
                },
                IRBasicBlock {
                    id: IRBlockId(1),
                    label: "if_then".to_string(),
                    params: Vec::new(),
                    instructions: vec![IRInstruction::Const {
                        dest: zero,
                        value: ConstValue::Int64(0),
                    }],
                    terminator: IRTerminator::Branch(BranchTarget::with_args(
                        IRBlockId(3),
                        vec![zero],
                    )),
                },
                IRBasicBlock {
                    id: IRBlockId(2),
                    label: "if_else".to_string(),
                    params: Vec::new(),
                    instructions: vec![
                        IRInstruction::LocalRead {
                            dest: read_dest,
                            local: param_local,
                            ty: param_ty.clone(),
                        },
                        IRInstruction::Call {
                            dest: call_dest,
                            callee: symbol.clone(),
                            args: vec![read_dest],
                        },
                    ],
                    terminator: IRTerminator::Branch(BranchTarget::with_args(
                        IRBlockId(3),
                        vec![call_dest],
                    )),
                },
                IRBasicBlock {
                    id: IRBlockId(3),
                    label: "if_merge".to_string(),
                    params: vec![BlockParam {
                        dest: merge_param,
                        ty: param_ty.clone(),
                    }],
                    instructions: Vec::new(),
                    terminator: IRTerminator::Return {
                        value: Some(merge_param),
                    },
                },
            ],
            kind: FunctionKind::Regular,
            params: vec![IRFunctionParam {
                id: param_id,
                local_id: param_local,
                ty: param_ty.clone(),
            }],
            return_type: param_ty,
            symbol,
        }
    }

    #[test]
    fn self_call_as_value_of_merge_rewrites_to_tail_call() {
        let function = build_wrapped_self_call(IRType::Int64);
        let symbol = function.symbol.clone();
        let mut packages = vec![package_with(function)];
        rewrite_tail_calls(&mut packages);
        let function = packages[0].functions.get(symbol.mangled()).unwrap();
        // The return-forwarding merge block is collapsed away.
        assert_eq!(
            function.blocks.len(),
            3,
            "merge block must be removed; blocks: {:?}",
            function.blocks.iter().map(|b| b.id).collect::<Vec<_>>(),
        );
        let recursive_arm = &function.blocks[2];
        let IRTerminator::TailCall { callee, args } = &recursive_arm.terminator else {
            panic!(
                "recursive arm must end in TailCall, got {:?}",
                recursive_arm.terminator,
            );
        };
        assert_eq!(callee, &symbol);
        assert_eq!(args, &vec![ValueId(3)], "scalar arg is forwarded as-is");
        assert!(
            !function
                .blocks
                .iter()
                .flat_map(|b| &b.instructions)
                .any(|inst| matches!(inst, IRInstruction::Call { .. })),
            "the self Call must be removed",
        );
        // The non-recursive arm now returns its value directly.
        assert!(matches!(
            function.blocks[1].terminator,
            IRTerminator::Return {
                value: Some(ValueId(2))
            }
        ));
    }

    /// A merge whose exit drops sit in the forwarder must have them
    /// replayed into each predecessor before the back-edge — the
    /// acquire of the heap arg still happens at the call site so the
    /// drop can't free storage the back-edge rebinds.
    #[test]
    fn heap_arg_through_merge_is_acquired_before_back_edge() {
        let function = build_wrapped_self_call(IRType::String);
        let symbol = function.symbol.clone();
        let mut packages = vec![package_with(function)];
        rewrite_tail_calls(&mut packages);
        let function = packages[0].functions.get(symbol.mangled()).unwrap();
        let recursive_arm = &function.blocks[2];
        let IRTerminator::TailCall { args, .. } = &recursive_arm.terminator else {
            panic!("expected TailCall, got {:?}", recursive_arm.terminator);
        };
        let arg = *args.first().expect("one arg");
        assert!(
            recursive_arm.instructions.iter().any(|inst| matches!(
                inst,
                IRInstruction::Clone { dest, .. } if *dest == arg
            )),
            "heap arg must be acquired before the back-edge: {:?}",
            recursive_arm.instructions,
        );
    }

    /// Nested merges (an inner `if`/`match` whose result feeds an outer
    /// merge) must peel to a fixpoint so a self-call buried two levels
    /// deep is still exposed.
    #[test]
    fn nested_merges_collapse_to_expose_inner_tail_call() {
        let symbol = IRSymbol::synthetic("Test.loop_nested".to_string());
        let param_id = ValueId(0);
        let param_local = local(0);
        let cond = ValueId(1);
        let zero = ValueId(2);
        let one = ValueId(3);
        let read_dest = ValueId(4);
        let call_dest = ValueId(5);
        let inner_param = ValueId(6);
        let outer_param = ValueId(7);
        let branch = |target: u32, arg: ValueId| {
            IRTerminator::Branch(BranchTarget::with_args(IRBlockId(target), vec![arg]))
        };
        let function = IRFunction {
            blocks: vec![
                IRBasicBlock {
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
                        IRInstruction::Const {
                            dest: cond,
                            value: ConstValue::Bool(true),
                        },
                    ],
                    terminator: IRTerminator::CondBranch {
                        cond,
                        else_target: BranchTarget::to(IRBlockId(2)),
                        then_target: BranchTarget::to(IRBlockId(1)),
                    },
                },
                IRBasicBlock {
                    id: IRBlockId(1),
                    label: "outer_then".to_string(),
                    params: Vec::new(),
                    instructions: vec![IRInstruction::Const {
                        dest: zero,
                        value: ConstValue::Int64(0),
                    }],
                    terminator: branch(6, zero),
                },
                IRBasicBlock {
                    id: IRBlockId(2),
                    label: "inner_if".to_string(),
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: IRTerminator::CondBranch {
                        cond,
                        else_target: BranchTarget::to(IRBlockId(4)),
                        then_target: BranchTarget::to(IRBlockId(3)),
                    },
                },
                IRBasicBlock {
                    id: IRBlockId(3),
                    label: "inner_then".to_string(),
                    params: Vec::new(),
                    instructions: vec![IRInstruction::Const {
                        dest: one,
                        value: ConstValue::Int64(1),
                    }],
                    terminator: branch(5, one),
                },
                IRBasicBlock {
                    id: IRBlockId(4),
                    label: "inner_else".to_string(),
                    params: Vec::new(),
                    instructions: vec![
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
                    terminator: branch(5, call_dest),
                },
                IRBasicBlock {
                    id: IRBlockId(5),
                    label: "inner_merge".to_string(),
                    params: vec![BlockParam {
                        dest: inner_param,
                        ty: IRType::Int64,
                    }],
                    instructions: Vec::new(),
                    terminator: branch(6, inner_param),
                },
                IRBasicBlock {
                    id: IRBlockId(6),
                    label: "outer_merge".to_string(),
                    params: vec![BlockParam {
                        dest: outer_param,
                        ty: IRType::Int64,
                    }],
                    instructions: Vec::new(),
                    terminator: IRTerminator::Return {
                        value: Some(outer_param),
                    },
                },
            ],
            kind: FunctionKind::Regular,
            params: vec![IRFunctionParam {
                id: param_id,
                local_id: param_local,
                ty: IRType::Int64,
            }],
            return_type: IRType::Int64,
            symbol: symbol.clone(),
        };
        let mut packages = vec![package_with(function)];
        rewrite_tail_calls(&mut packages);
        let function = packages[0].functions.get(symbol.mangled()).unwrap();
        // Both merge blocks peel away.
        assert_eq!(function.blocks.len(), 5);
        let inner_else = function
            .blocks
            .iter()
            .find(|b| b.label == "inner_else")
            .unwrap();
        assert!(
            matches!(inner_else.terminator, IRTerminator::TailCall { .. }),
            "self-call two merges deep must be rewritten; got {:?}",
            inner_else.terminator,
        );
    }

    /// A merge reached by a `CondBranch` edge (a no-`else` `if` whose
    /// false edge targets the merge directly) is left intact: rewriting
    /// only one edge of a two-way branch into a `Return` is impossible,
    /// so the conservative path keeps the merge and forgoes TCO rather
    /// than stranding a block param.
    #[test]
    fn condbranch_into_merge_is_left_intact() {
        let symbol = IRSymbol::synthetic("Test.loop_no_else".to_string());
        let param_id = ValueId(0);
        let param_local = local(0);
        let cond = ValueId(1);
        let read_dest = ValueId(2);
        let call_dest = ValueId(3);
        let unit = ValueId(4);
        let merge_param = ValueId(5);
        let function = IRFunction {
            blocks: vec![
                IRBasicBlock {
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
                        IRInstruction::Const {
                            dest: cond,
                            value: ConstValue::Bool(true),
                        },
                        IRInstruction::Const {
                            dest: unit,
                            value: ConstValue::Int64(0),
                        },
                    ],
                    terminator: IRTerminator::CondBranch {
                        cond,
                        else_target: BranchTarget::with_args(IRBlockId(2), vec![unit]),
                        then_target: BranchTarget::to(IRBlockId(1)),
                    },
                },
                IRBasicBlock {
                    id: IRBlockId(1),
                    label: "if_then".to_string(),
                    params: Vec::new(),
                    instructions: vec![
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
                    terminator: IRTerminator::Branch(BranchTarget::with_args(
                        IRBlockId(2),
                        vec![call_dest],
                    )),
                },
                IRBasicBlock {
                    id: IRBlockId(2),
                    label: "if_merge".to_string(),
                    params: vec![BlockParam {
                        dest: merge_param,
                        ty: IRType::Int64,
                    }],
                    instructions: Vec::new(),
                    terminator: IRTerminator::Return {
                        value: Some(merge_param),
                    },
                },
            ],
            kind: FunctionKind::Regular,
            params: vec![IRFunctionParam {
                id: param_id,
                local_id: param_local,
                ty: IRType::Int64,
            }],
            return_type: IRType::Int64,
            symbol: symbol.clone(),
        };
        let mut packages = vec![package_with(function)];
        rewrite_tail_calls(&mut packages);
        let function = packages[0].functions.get(symbol.mangled()).unwrap();
        assert_eq!(function.blocks.len(), 3, "merge must be kept intact");
        assert!(
            !function
                .blocks
                .iter()
                .any(|b| matches!(b.terminator, IRTerminator::TailCall { .. })),
            "no TCO when the merge has a CondBranch predecessor",
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
