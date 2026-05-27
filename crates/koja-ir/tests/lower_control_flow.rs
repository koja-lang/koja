//! Coverage for `if` / `unless` / `cond` lowering in
//! `src/lower/control_flow.rs`.
//!
//! Pins the basic-block CFG shape for the block-parameter SSA join
//! model: every value-producing conditional ends in a merge block
//! whose [`BlockParam`] receives each reaching arm's tail value via
//! the per-edge [`BranchTarget::args`] payload. Backends translate
//! the block param to a phi node (LLVM) or bind on edge traversal
//! (eval); this test crate just inspects the IR shape.
//!
//! [`BlockParam`]: koja_ir::BlockParam
//! [`BranchTarget::args`]: koja_ir::BranchTarget::args

use koja_ast::util::dedent;
use koja_ir::{BranchTarget, ConstValue, IRInstruction, IRTerminator, IRType};

mod common;

use common::{function, lower_program_source as lower};

#[test]
fn if_no_else_lowers_to_three_blocks_with_unit_merge_param() {
    let source = "
        fn cond_true -> Bool
          true
        end

        fn main
          if cond_true()
            1
          end
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    assert_eq!(
        main.blocks.len(),
        3,
        "expected entry/then/merge blocks; got {} blocks",
        main.blocks.len(),
    );

    let entry = &main.blocks[0];
    let IRTerminator::CondBranch {
        cond: _,
        else_target,
        then_target,
    } = &entry.terminator
    else {
        panic!(
            "expected entry to terminate in CondBranch; got {:?}",
            entry.terminator
        );
    };

    let then_block = main
        .blocks
        .iter()
        .find(|b| b.id == then_target.block)
        .expect("then-block missing");
    let merge_block = main
        .blocks
        .iter()
        .find(|b| b.id == else_target.block)
        .expect("merge-block missing");

    // Then-target carries no per-edge args (the cond=true edge runs
    // the body block, which then branches into merge with its own
    // tail value).
    assert!(
        then_target.args.is_empty(),
        "then-target should branch to body block with no args; got {:?}",
        then_target.args,
    );
    // Else-target bypasses the body and passes a synthesized
    // `Const::Unit` directly to the merge block.
    assert_eq!(
        else_target.args.len(),
        1,
        "else-target should pass one Unit arg to the merge block; got {:?}",
        else_target.args,
    );

    // Merge has one Unit-typed BlockParam.
    assert_eq!(
        merge_block.params.len(),
        1,
        "merge block should declare exactly one BlockParam; got {} params",
        merge_block.params.len(),
    );
    assert_eq!(
        merge_block.params[0].ty,
        IRType::Unit,
        "merge BlockParam should be Unit-typed for if-no-else",
    );
    let merge_param = merge_block.params[0].dest;

    // Then-block branches into merge with its tail (coerced to Unit
    // for the no-else / Unit-typed case).
    let then_term = match &then_block.terminator {
        IRTerminator::Branch(target) => target,
        other => panic!("then-block should end in Branch; got {other:?}"),
    };
    assert_eq!(then_term.block, merge_block.id);
    assert_eq!(
        then_term.args.len(),
        1,
        "then-block should branch into merge with one arg",
    );

    // The function's trailing Return reads the merge's BlockParam.
    assert_eq!(
        merge_block.terminator,
        IRTerminator::Return {
            value: Some(merge_param)
        },
        "merge block should `Return` its BlockParam value",
    );
}

#[test]
fn if_else_lowers_to_four_blocks_with_typed_merge_param() {
    let source = "
        fn pick -> Int
          if true
            1
          else
            2
          end
        end

        fn main
          pick()
        end
        ";

    let program = lower(&dedent(source));
    let pick = function(&program, "pick");
    assert_eq!(
        pick.blocks.len(),
        4,
        "expected entry/then/else/merge blocks; got {} blocks",
        pick.blocks.len(),
    );

    let entry = &pick.blocks[0];
    let IRTerminator::CondBranch {
        else_target,
        then_target,
        ..
    } = &entry.terminator
    else {
        panic!("entry should end in CondBranch; got {:?}", entry.terminator);
    };
    assert!(then_target.args.is_empty());
    assert!(else_target.args.is_empty());

    let then_block = pick
        .blocks
        .iter()
        .find(|b| b.id == then_target.block)
        .expect("then-block missing");
    let else_block = pick
        .blocks
        .iter()
        .find(|b| b.id == else_target.block)
        .expect("else-block missing");

    // Both arms branch into the same merge block with their tail
    // values as the per-edge arg.
    let then_term = match &then_block.terminator {
        IRTerminator::Branch(target) => target,
        other => panic!("then-block should end in Branch; got {other:?}"),
    };
    let else_term = match &else_block.terminator {
        IRTerminator::Branch(target) => target,
        other => panic!("else-block should end in Branch; got {other:?}"),
    };
    assert_eq!(then_term.block, else_term.block, "arms must share a merge");

    let merge_block = pick
        .blocks
        .iter()
        .find(|b| b.id == then_term.block)
        .expect("merge-block missing");
    assert_eq!(merge_block.params.len(), 1);
    assert_eq!(
        merge_block.params[0].ty,
        IRType::Int64,
        "merge BlockParam should be Int (Int64) typed for an Int-valued if/else",
    );
    assert_eq!(then_term.args.len(), 1);
    assert_eq!(else_term.args.len(), 1);
}

#[test]
fn unless_swaps_then_and_else_relative_to_if() {
    let source = "
        fn cond_false -> Bool
          false
        end

        fn main
          unless cond_false()
            1
          end
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    let entry = &main.blocks[0];
    let IRTerminator::CondBranch {
        else_target,
        then_target,
        ..
    } = &entry.terminator
    else {
        panic!(
            "expected entry to terminate in CondBranch; got {:?}",
            entry.terminator
        );
    };

    // For `unless`, cond=true bypasses to merge with `Unit` (then
    // target has args), cond=false runs the body (else target has
    // none).
    assert_eq!(
        then_target.args.len(),
        1,
        "unless's then-target should bypass body and carry a Unit arg",
    );
    assert!(
        else_target.args.is_empty(),
        "unless's else-target should branch to the body block with no args",
    );

    let merge_block = main
        .blocks
        .iter()
        .find(|b| b.id == then_target.block)
        .expect("merge-block missing");
    assert_eq!(merge_block.label, "unless_merge");
    assert_eq!(merge_block.params.len(), 1);
}

#[test]
fn if_function_returns_unit() {
    let source = "
        fn cond_true -> Bool
          true
        end

        fn main
          if cond_true()
            1
          end
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    assert_eq!(
        main.return_type,
        IRType::Unit,
        "if without else is Unit-typed",
    );
}

#[test]
fn early_return_inside_if_closes_then_branch() {
    let source = "
        fn pick -> Int
          if true
            return 1
          end
          2
        end

        fn main
          pick()
        end
        ";

    let program = lower(&dedent(source));
    let pick = function(&program, "pick");
    let return_blocks: Vec<_> = pick
        .blocks
        .iter()
        .filter(|b| matches!(b.terminator, IRTerminator::Return { value: Some(_) }))
        .collect();
    assert_eq!(
        return_blocks.len(),
        2,
        "expected two Return-terminated blocks (then-branch's early return + merge fall-through); \
         got {return_blocks:#?}",
    );
}

#[test]
fn cond_lowers_to_chained_test_blocks_with_typed_merge_param() {
    let source = "
        fn pick -> Int
          cond
            true -> 1
            false -> 2
            else -> 3
          end
        end

        fn main
          pick()
        end
        ";

    let program = lower(&dedent(source));
    let pick = function(&program, "pick");
    let merge = pick
        .blocks
        .iter()
        .find(|b| b.label == "cond_merge")
        .expect("missing cond_merge block");
    assert_eq!(merge.params.len(), 1);
    assert_eq!(
        merge.params[0].ty,
        IRType::Int64,
        "cond merge BlockParam should be Int-typed",
    );

    // Three branches into the merge: two arm bodies + one else body.
    let incoming: Vec<&BranchTarget> = pick
        .blocks
        .iter()
        .filter_map(|b| match &b.terminator {
            IRTerminator::Branch(t) if t.block == merge.id => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(
        incoming.len(),
        3,
        "expected three branches into cond_merge (one per arm + else); got {incoming:?}",
    );
    for target in &incoming {
        assert_eq!(
            target.args.len(),
            1,
            "every cond arm should pass a single Int-typed arg to the merge",
        );
    }
}

#[test]
fn ternary_emits_cond_branch_to_two_arms_and_merge_block_param() {
    // Same shape as `if`/`else`'s with-else path: entry CondBranches
    // to a then- and an else-block, both arms unconditionally branch
    // into a typed merge block, and the merge's BlockParam is the
    // ternary's value.
    let source = "
        fn pick -> Int
          true ? 1 : 2
        end

        fn main
          pick()
        end
        ";

    let program = lower(&dedent(source));
    let pick = function(&program, "pick");
    assert_eq!(
        pick.blocks.len(),
        4,
        "expected entry/then/else/merge blocks; got {} blocks",
        pick.blocks.len(),
    );

    let entry = &pick.blocks[0];
    let IRTerminator::CondBranch {
        else_target,
        then_target,
        ..
    } = &entry.terminator
    else {
        panic!("entry should end in CondBranch; got {:?}", entry.terminator);
    };
    assert!(
        then_target.args.is_empty(),
        "ternary then-target branches to body block with no args; got {:?}",
        then_target.args,
    );
    assert!(
        else_target.args.is_empty(),
        "ternary else-target branches to body block with no args; got {:?}",
        else_target.args,
    );

    let then_block = pick
        .blocks
        .iter()
        .find(|b| b.id == then_target.block)
        .expect("then-block missing");
    let else_block = pick
        .blocks
        .iter()
        .find(|b| b.id == else_target.block)
        .expect("else-block missing");
    assert_eq!(then_block.label, "ternary_then");
    assert_eq!(else_block.label, "ternary_else");

    let then_term = match &then_block.terminator {
        IRTerminator::Branch(target) => target,
        other => panic!("then-block should end in Branch; got {other:?}"),
    };
    let else_term = match &else_block.terminator {
        IRTerminator::Branch(target) => target,
        other => panic!("else-block should end in Branch; got {other:?}"),
    };
    assert_eq!(
        then_term.block, else_term.block,
        "ternary arms must share a merge block",
    );

    let merge = pick
        .blocks
        .iter()
        .find(|b| b.id == then_term.block)
        .expect("merge-block missing");
    assert_eq!(merge.label, "ternary_merge");
    assert_eq!(merge.params.len(), 1);
    assert_eq!(
        merge.params[0].ty,
        IRType::Int64,
        "ternary merge BlockParam should be Int-typed for an Int-valued ternary",
    );
    assert_eq!(then_term.args.len(), 1);
    assert_eq!(else_term.args.len(), 1);
}

#[test]
fn merge_block_param_value_drives_function_return() {
    // The trailing if/else expression's value is the merge block's
    // BlockParam; the function-end Return reads that param.
    let source = "
        fn pick -> Int
          if true
            1
          else
            2
          end
        end

        fn main
          pick()
        end
        ";

    let program = lower(&dedent(source));
    let pick = function(&program, "pick");
    let merge = pick
        .blocks
        .iter()
        .find(|b| b.label == "if_merge")
        .expect("missing if_merge block");
    let merge_param = merge.params[0].dest;
    assert_eq!(
        merge.terminator,
        IRTerminator::Return {
            value: Some(merge_param)
        },
        "merge's `Return` should read the BlockParam carrying the joined arm value",
    );
    // Sanity: no Const::Unit emitted in the merge — the arms hand
    // it a real Int via the BlockParam.
    let unit_const_in_merge = merge.instructions.iter().any(|i| {
        matches!(
            i,
            IRInstruction::Const {
                value: ConstValue::Unit,
                ..
            }
        )
    });
    assert!(
        !unit_const_in_merge,
        "merge block of an Int-valued if/else should not synthesize a Unit constant",
    );
}
