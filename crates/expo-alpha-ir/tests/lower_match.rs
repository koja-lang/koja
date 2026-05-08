//! Coverage for `match` lowering in `src/lower/match_expr.rs`.
//!
//! Pins the linear-arm-chain CFG: each non-catch-all arm runs a
//! pattern test in its own block, cond=false falls through to the
//! next arm's test, and every arm body branches into one merge
//! block carrying the join value as a typed [`BlockParam`]. The
//! catch-all arm closes the chain with an unconditional `Branch`
//! to its body block. A guarded arm interposes a `match_guard_<n>`
//! block between pattern success and the body — payload binds
//! land at the head of the guard block, the guard expr runs there,
//! and the block ends in a `CondBranch` to the body or the same
//! fall-through the pattern's failure edge uses.
//!
//! [`BlockParam`]: expo_alpha_ir::BlockParam

use expo_alpha_ir::{ConstValue, IRBinOp, IRInstruction, IRTerminator, IRType, IRVariantTag};
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

#[test]
fn match_enum_unit_arm_emits_tag_get_and_eq_chain() {
    let source = "
        enum Color
          Red
          Green
        end

        fn pick(c: Color) -> Int
          match c
            Color.Red -> 1
            Color.Green -> 2
          end
        end

        fn main
          pick(Color.Red)
        end
        ";

    let program = lower(&dedent(source));
    let pick = function(&program, "pick");

    let entry = &pick.blocks[0];
    let has_tag_get = entry
        .instructions
        .iter()
        .any(|i| matches!(i, IRInstruction::EnumTagGet { .. }));
    assert!(
        has_tag_get,
        "enum-unit pattern should emit `EnumTagGet` against the subject; \
         got: {:?}",
        entry.instructions,
    );
    let has_int8_const = entry.instructions.iter().any(|i| {
        matches!(
            i,
            IRInstruction::Const {
                value: ConstValue::Int8(_),
                ..
            }
        )
    });
    assert!(
        has_int8_const,
        "enum-unit pattern should emit `Const::Int8` for the variant tag",
    );
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
        "enum-unit pattern should chain a `BinaryOp::Eq` between the tag and the const",
    );
}

#[test]
fn match_enum_tuple_binding_emits_payload_field_get_and_local_write() {
    let source = "
        enum Box
          Some(Int)
          None
        end

        fn unwrap(b: Box) -> Int
          match b
            Box.Some(x) -> x
            Box.None -> 0
          end
        end

        fn main
          unwrap(Box.Some(7))
        end
        ";

    let program = lower(&dedent(source));
    let unwrap_fn = function(&program, "unwrap");

    let payload_get = unwrap_fn.blocks.iter().find_map(|b| {
        b.instructions.iter().find_map(|i| match i {
            IRInstruction::EnumPayloadFieldGet {
                payload_index, tag, ..
            } => Some((*tag, *payload_index)),
            _ => None,
        })
    });
    let (tag, payload_index) =
        payload_get.expect("payload binding should emit `EnumPayloadFieldGet`");
    assert_eq!(
        tag,
        IRVariantTag(0),
        "Some is the first declared variant — tag 0",
    );
    assert_eq!(payload_index, 0, "x is the first payload field");

    let body_block = unwrap_fn
        .blocks
        .iter()
        .find(|b| b.label == "match_body_0")
        .expect("missing match_body_0 block");
    let body_has_payload_get = body_block
        .instructions
        .iter()
        .any(|i| matches!(i, IRInstruction::EnumPayloadFieldGet { .. }));
    assert!(
        body_has_payload_get,
        "payload field-get must run on the success edge — appears in the arm body block",
    );
    let body_has_local_write = body_block
        .instructions
        .iter()
        .any(|i| matches!(i, IRInstruction::LocalWrite { .. }));
    assert!(
        body_has_local_write,
        "binding should emit a `LocalWrite` in the body block",
    );

    let entry = &unwrap_fn.blocks[0];
    let entry_has_local_decl = entry
        .instructions
        .iter()
        .any(|i| matches!(i, IRInstruction::LocalDecl { .. }));
    assert!(
        entry_has_local_decl,
        "binding's `LocalDecl` should be hoisted to the function entry block",
    );
}

#[test]
fn match_exhaustive_enum_synthesizes_unreachable_trap_block() {
    let source = "
        enum Color
          Red
          Green
        end

        fn pick(c: Color) -> Int
          match c
            Color.Red -> 1
            Color.Green -> 2
          end
        end

        fn main
          pick(Color.Red)
        end
        ";

    let program = lower(&dedent(source));
    let pick = function(&program, "pick");

    let unreachable_block = pick
        .blocks
        .iter()
        .find(|b| b.terminator == IRTerminator::Unreachable)
        .expect(
            "exhaustive enum match without a catch-all should synthesize an `Unreachable` \
             trap block on the last arm's failure edge",
        );
    assert_eq!(
        unreachable_block.label, "match_unreachable",
        "trap block should carry the canonical `match_unreachable` label",
    );
}

#[test]
fn match_or_alternatives_chain_through_dedicated_test_blocks() {
    let source = "
        fn pick -> Int
          match \"a\"
            \"a\" | \"b\" | \"c\" -> 1
            _ -> 0
          end
        end

        fn main
          pick()
        end
        ";

    let program = lower(&dedent(source));
    let pick = function(&program, "pick");

    let alt_count = pick
        .blocks
        .iter()
        .filter(|b| b.label.starts_with("match_or_alt_"))
        .count();
    assert_eq!(
        alt_count, 2,
        "an or-pattern of 3 alternatives should mint 2 fresh chain blocks (the first \
         alternative emits into the existing test block); got {alt_count}",
    );
    for alt_block in pick
        .blocks
        .iter()
        .filter(|b| b.label.starts_with("match_or_alt_"))
    {
        assert!(
            matches!(alt_block.terminator, IRTerminator::CondBranch { .. }),
            "every or-alternative test block should terminate in a `CondBranch`; \
             got {:?}",
            alt_block.terminator,
        );
    }
}

#[test]
fn match_guarded_arm_interposes_guard_block_between_test_and_body() {
    let source = "
        fn pick(n: Int) -> Int
          match n
            x when x > 0 -> 10
            _ -> 20
          end
        end

        fn main
          pick(7)
        end
        ";

    let program = lower(&dedent(source));
    let pick = function(&program, "pick");

    let guard_block = pick
        .blocks
        .iter()
        .find(|b| b.label == "match_guard_0")
        .expect("guarded arm should mint a `match_guard_0` block");

    let IRTerminator::CondBranch {
        then_target,
        else_target,
        ..
    } = &guard_block.terminator
    else {
        panic!(
            "guard block should terminate in a `CondBranch`; got {:?}",
            guard_block.terminator,
        );
    };
    let body_block = pick
        .blocks
        .iter()
        .find(|b| b.label == "match_body_0")
        .expect("missing match_body_0");
    assert_eq!(
        then_target.block, body_block.id,
        "guard true should branch into the arm body block",
    );
    let fall_through = pick
        .blocks
        .iter()
        .find(|b| b.label == "match_body_1")
        .expect("missing match_body_1 (the catch-all body)");
    let next_test = pick.blocks.iter().find(|b| b.label == "match_test_1");
    let expected_else = next_test.map(|b| b.id).unwrap_or(fall_through.id);
    assert_eq!(
        else_target.block, expected_else,
        "guard false should fall through to the next arm's test (or its body when the \
         catch-all is the next arm)",
    );

    let body_has_local_write = body_block
        .instructions
        .iter()
        .any(|i| matches!(i, IRInstruction::LocalWrite { .. }));
    assert!(
        !body_has_local_write,
        "the body block should not host a `LocalWrite` for a guarded binding — the \
         binding writes upstream so the guard sees it",
    );
}

#[test]
fn match_guarded_enum_payload_binds_land_in_guard_block() {
    let source = "
        enum Box
          Some(Int)
          None
        end

        fn unwrap(b: Box) -> Int
          match b
            Box.Some(x) when x > 0 -> x
            _ -> 0
          end
        end

        fn main
          unwrap(Box.Some(7))
        end
        ";

    let program = lower(&dedent(source));
    let unwrap_fn = function(&program, "unwrap");

    let guard_block = unwrap_fn
        .blocks
        .iter()
        .find(|b| b.label == "match_guard_0")
        .expect("guarded enum-tuple arm should mint a `match_guard_0` block");
    let guard_has_payload_get = guard_block
        .instructions
        .iter()
        .any(|i| matches!(i, IRInstruction::EnumPayloadFieldGet { .. }));
    assert!(
        guard_has_payload_get,
        "payload-field-get must run in the guard block so the guard expr sees the binding",
    );
}
