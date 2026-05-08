//! Coverage for `match` lowering in `src/lower/match_expr.rs`.
//!
//! Pins the linear-arm-chain CFG: each non-catch-all arm runs a
//! pattern test in its own block, cond=false falls through to the
//! next arm's test, and every arm body branches into one merge
//! block carrying the join value as a typed [`BlockParam`]. The
//! catch-all arm closes the chain with an unconditional `Branch`
//! to its body block.
//!
//! [`BlockParam`]: expo_alpha_ir::BlockParam

use expo_alpha_ir::{ConstValue, IRBinOp, IRInstruction, IRTerminator, IRType};
use expo_ast::util::dedent;

mod common;

use common::{function, lower_program_source as lower};

#[test]
fn match_int_literal_chain_lowers_to_test_blocks_and_typed_merge() {
    let source = "
        fn pick -> Int
          match 1
            1 -> 10
            2 -> 20
            _ -> 30
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
        .find(|b| b.label == "match_merge")
        .expect("missing match_merge block");
    assert_eq!(
        merge.params.len(),
        1,
        "match merge should declare exactly one BlockParam",
    );
    assert_eq!(
        merge.params[0].ty,
        IRType::Int64,
        "match merge BlockParam should be Int64-typed for an Int-valued match",
    );

    let merge_param = merge.params[0].dest;
    assert_eq!(
        merge.terminator,
        IRTerminator::Return {
            value: Some(merge_param),
        },
        "merge's `Return` should read the joined arm value via the BlockParam",
    );

    let body_count = pick
        .blocks
        .iter()
        .filter(|b| b.label.starts_with("match_body_"))
        .count();
    assert_eq!(
        body_count, 3,
        "expected one body block per arm; got {body_count}",
    );

    let test_count = pick
        .blocks
        .iter()
        .filter(|b| b.label.starts_with("match_test_"))
        .count();
    assert_eq!(
        test_count, 2,
        "expected one chained test block per non-first arm; got {test_count}",
    );

    let incoming_to_merge: Vec<_> = pick
        .blocks
        .iter()
        .filter_map(|b| match &b.terminator {
            IRTerminator::Branch(target) if target.block == merge.id => Some(target),
            _ => None,
        })
        .collect();
    assert_eq!(
        incoming_to_merge.len(),
        3,
        "expected three branches into match_merge (one per arm body); got {incoming_to_merge:?}",
    );
    for target in &incoming_to_merge {
        assert_eq!(
            target.args.len(),
            1,
            "every arm body should pass one Int arg to the merge block",
        );
    }
}

#[test]
fn match_literal_arm_emits_subject_eq_const_predicate() {
    let source = "
        fn pick -> Int
          match 1
            1 -> 10
            _ -> 20
          end
        end

        fn main
          pick()
        end
        ";

    let program = lower(&dedent(source));
    let pick = function(&program, "pick");

    let entry = &pick.blocks[0];
    let has_eq = entry.instructions.iter().any(|i| {
        matches!(
            i,
            IRInstruction::BinaryOp {
                op: IRBinOp::Eq,
                ..
            }
        )
    });
    assert!(
        has_eq,
        "first arm's literal pattern should emit `BinaryOp::Eq` against the subject in the entry block; \
         got instructions: {:?}",
        entry.instructions,
    );
    let IRTerminator::CondBranch { .. } = &entry.terminator else {
        panic!(
            "first arm's test block should end in CondBranch; got {:?}",
            entry.terminator,
        );
    };
}

#[test]
fn match_catch_all_branches_unconditionally_to_body() {
    let source = "
        fn pick -> Int
          match 1
            _ -> 42
          end
        end

        fn main
          pick()
        end
        ";

    let program = lower(&dedent(source));
    let pick = function(&program, "pick");

    let entry = &pick.blocks[0];
    let IRTerminator::Branch(target) = &entry.terminator else {
        panic!(
            "single-catch-all match should terminate the test block in an unconditional Branch; \
             got {:?}",
            entry.terminator,
        );
    };

    let body = pick
        .blocks
        .iter()
        .find(|b| b.id == target.block)
        .expect("body-block missing");
    assert_eq!(body.label, "match_body_0");
}

#[test]
fn match_binding_emits_local_decl_and_write() {
    let source = "
        fn pick -> Int
          match 7
            x -> x
          end
        end

        fn main
          pick()
        end
        ";

    let program = lower(&dedent(source));
    let pick = function(&program, "pick");

    let has_decl = pick.blocks.iter().any(|b| {
        b.instructions
            .iter()
            .any(|i| matches!(i, IRInstruction::LocalDecl { .. }))
    });
    assert!(
        has_decl,
        "match binding `x` should emit a `LocalDecl` (in the function entry block)",
    );

    let has_write = pick.blocks.iter().any(|b| {
        b.instructions
            .iter()
            .any(|i| matches!(i, IRInstruction::LocalWrite { .. }))
    });
    assert!(
        has_write,
        "match binding `x` should emit a `LocalWrite` capturing the subject value",
    );
}

#[test]
fn match_string_literal_arm_lowers_const_string_and_eq() {
    let source = "
        fn pick -> Int
          match \"hi\"
            \"hi\" -> 1
            _ -> 0
          end
        end

        fn main
          pick()
        end
        ";

    let program = lower(&dedent(source));
    let pick = function(&program, "pick");

    let entry = &pick.blocks[0];
    let has_string_const = entry.instructions.iter().any(|i| {
        matches!(
            i,
            IRInstruction::Const {
                value: ConstValue::String(_),
                ..
            }
        )
    });
    assert!(
        has_string_const,
        "string-literal pattern should emit a `Const::String` for the comparand; \
         got: {:?}",
        entry.instructions,
    );
    let has_string_eq = entry.instructions.iter().any(|i| {
        matches!(
            i,
            IRInstruction::BinaryOp {
                op: IRBinOp::Eq,
                ..
            }
        )
    });
    assert!(
        has_string_eq,
        "string-literal pattern should compare with `BinaryOp::Eq`",
    );
}
