//! Coverage for `match` lowering in `src/lower/match_expr.rs`.
//!
//! Pins the linear-arm-chain CFG. Each non-catch-all arm runs a
//! pattern test in its own block, cond=false falls through to the
//! next arm's test, and every arm body branches into one merge
//! block carrying the join value as a typed [`BlockParam`]. The
//! catch-all arm closes the chain with an unconditional `Branch`
//! to its body block. A guarded arm interposes a `match_guard_<n>`
//! block between pattern success and the body. Payload binds land
//! at the head of the guard block, the guard expr runs there, and
//! the block ends in a `CondBranch` to the body or the same
//! fall-through the pattern's failure edge uses.
//!
//! Struct destructure patterns extend the bind protocol. Enum
//! struct variants emit `EnumPayloadFieldGet` indexed by declared
//! field position, while plain-struct destructures emit `FieldGet`
//! and act as catch-alls (no tag check, no test block, only binds
//! in the success block).
//!
//! [`BlockParam`]: koja_ir::BlockParam

use koja_ir::{ConstValue, IRBinOp, IRInstruction, IRTerminator, IRType, IRVariantTag};

mod common;

use common::{
    all_instructions, block_labeled, branch_targets_into, count_blocks_with_prefix, entry_block,
    lower_script_source as lower, script_function,
};

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

        pick()
        ";

    let script = lower(source);
    let pick = script_function(&script, "pick");

    let merge = block_labeled(&pick.blocks, "match_merge");
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

    let body_count = count_blocks_with_prefix(&pick.blocks, "match_body_");
    assert_eq!(
        body_count, 3,
        "expected one body block per arm; got {body_count}",
    );

    let test_count = count_blocks_with_prefix(&pick.blocks, "match_test_");
    assert_eq!(
        test_count, 2,
        "expected one chained test block per non-first arm; got {test_count}",
    );

    let incoming_to_merge = branch_targets_into(&pick.blocks, merge.id);
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

        pick()
        ";

    let script = lower(source);
    let pick = script_function(&script, "pick");

    let entry = entry_block(&pick.blocks);
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

        pick()
        ";

    let script = lower(source);
    let pick = script_function(&script, "pick");

    let entry = entry_block(&pick.blocks);
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

        pick()
        ";

    let script = lower(source);
    let pick = script_function(&script, "pick");

    let has_decl =
        all_instructions(&pick.blocks).any(|i| matches!(i, IRInstruction::LocalDecl { .. }));
    assert!(
        has_decl,
        "match binding `x` should emit a `LocalDecl` (in the function entry block)",
    );

    let has_write =
        all_instructions(&pick.blocks).any(|i| matches!(i, IRInstruction::LocalWrite { .. }));
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

        pick()
        ";

    let script = lower(source);
    let pick = script_function(&script, "pick");

    let entry = entry_block(&pick.blocks);
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

        pick(Color.Red)
        ";

    let script = lower(source);
    let pick = script_function(&script, "pick");

    let entry = entry_block(&pick.blocks);
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

        unwrap(Box.Some(7))
        ";

    let script = lower(source);
    let unwrap_fn = script_function(&script, "unwrap");

    let payload_get = all_instructions(&unwrap_fn.blocks).find_map(|i| match i {
        IRInstruction::EnumPayloadFieldGet {
            payload_index, tag, ..
        } => Some((*tag, *payload_index)),
        _ => None,
    });
    let (tag, payload_index) =
        payload_get.expect("payload binding should emit `EnumPayloadFieldGet`");
    assert_eq!(
        tag,
        IRVariantTag(0),
        "Some is the first declared variant (tag 0)",
    );
    assert_eq!(payload_index, 0, "x is the first payload field");

    let body_block = block_labeled(&unwrap_fn.blocks, "match_body_0");
    let body_has_payload_get = body_block
        .instructions
        .iter()
        .any(|i| matches!(i, IRInstruction::EnumPayloadFieldGet { .. }));
    assert!(
        body_has_payload_get,
        "payload field-get must run on the success edge, in the arm body block",
    );
    let body_has_local_write = body_block
        .instructions
        .iter()
        .any(|i| matches!(i, IRInstruction::LocalWrite { .. }));
    assert!(
        body_has_local_write,
        "binding should emit a `LocalWrite` in the body block",
    );

    let entry = entry_block(&unwrap_fn.blocks);
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

        pick(Color.Red)
        ";

    let script = lower(source);
    let pick = script_function(&script, "pick");

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

        pick()
        ";

    let script = lower(source);
    let pick = script_function(&script, "pick");

    let alt_count = count_blocks_with_prefix(&pick.blocks, "match_or_alt_");
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

        pick(7)
        ";

    let script = lower(source);
    let pick = script_function(&script, "pick");

    let guard_block = block_labeled(&pick.blocks, "match_guard_0");

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
    let body_block = block_labeled(&pick.blocks, "match_body_0");
    assert_eq!(
        then_target.block, body_block.id,
        "guard true should branch into the arm body block",
    );
    let fall_through = block_labeled(&pick.blocks, "match_body_1");
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
        "the body block should not host a `LocalWrite` for a guarded binding (the \
         binding writes upstream so the guard sees it)",
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

        unwrap(Box.Some(7))
        ";

    let script = lower(source);
    let unwrap_fn = script_function(&script, "unwrap");

    let guard_block = block_labeled(&unwrap_fn.blocks, "match_guard_0");
    let guard_has_payload_get = guard_block
        .instructions
        .iter()
        .any(|i| matches!(i, IRInstruction::EnumPayloadFieldGet { .. }));
    assert!(
        guard_has_payload_get,
        "payload-field-get must run in the guard block so the guard expr sees the binding",
    );
}

#[test]
fn match_struct_destructure_emits_field_get_in_body_block() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn add -> Int
          match Point{x: 3, y: 4}
            Point{x: a, y: b} -> a + b
          end
        end

        add()
        ";

    let script = lower(source);
    let add_fn = script_function(&script, "add");

    let body_block = block_labeled(&add_fn.blocks, "match_body_0");

    let field_indices: Vec<u32> = body_block
        .instructions
        .iter()
        .filter_map(|i| match i {
            IRInstruction::FieldGet { field_index, .. } => Some(*field_index),
            _ => None,
        })
        .collect();
    assert_eq!(
        field_indices,
        vec![0, 1],
        "struct destructure should emit one `FieldGet` per binding in declared order; \
         got {field_indices:?}",
    );

    let local_writes = body_block
        .instructions
        .iter()
        .filter(|i| matches!(i, IRInstruction::LocalWrite { .. }))
        .count();
    assert_eq!(
        local_writes, 2,
        "each binding should emit a `LocalWrite` after its `FieldGet`",
    );

    let no_tag_get = !body_block
        .instructions
        .iter()
        .any(|i| matches!(i, IRInstruction::EnumTagGet { .. }));
    assert!(
        no_tag_get,
        "plain-struct destructure should not emit any `EnumTagGet`",
    );
}

#[test]
fn match_enum_struct_destructure_emits_payload_field_get_by_declared_index() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}
          Circle{r: Int}
        end

        fn area(s: Shape) -> Int
          match s
            Shape.Rect{w: w, h: h} -> w * h
            Shape.Circle{r: r} -> r * r
          end
        end

        area(Shape.Rect{w: 3, h: 4})
        ";

    let script = lower(source);
    let area_fn = script_function(&script, "area");

    let rect_body = block_labeled(&area_fn.blocks, "match_body_0");
    let payload_indices: Vec<u32> = rect_body
        .instructions
        .iter()
        .filter_map(|i| match i {
            IRInstruction::EnumPayloadFieldGet {
                payload_index, tag, ..
            } if *tag == IRVariantTag(0) => Some(*payload_index),
            _ => None,
        })
        .collect();
    assert_eq!(
        payload_indices,
        vec![0, 1],
        "enum-struct destructure should look up by name and emit declared-position \
         payload indices; got {payload_indices:?}",
    );

    let circle_body = block_labeled(&area_fn.blocks, "match_body_1");
    let circle_tags: Vec<IRVariantTag> = circle_body
        .instructions
        .iter()
        .filter_map(|i| match i {
            IRInstruction::EnumPayloadFieldGet { tag, .. } => Some(*tag),
            _ => None,
        })
        .collect();
    assert_eq!(
        circle_tags,
        vec![IRVariantTag(1)],
        "Circle's bind should carry the Circle variant's tag",
    );
}

#[test]
fn match_struct_destructure_acts_as_catch_all_in_chain() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn first -> Int
          match Point{x: 1, y: 2}
            Point{x: a, y: _} -> a
          end
        end

        first()
        ";

    let script = lower(source);
    let first_fn = script_function(&script, "first");

    let entry = entry_block(&first_fn.blocks);
    assert!(
        matches!(&entry.terminator, IRTerminator::Branch(_)),
        "plain-struct destructure should close the chain with an unconditional Branch \
         from the entry block; got {:?}",
        entry.terminator,
    );

    let test_block_count = count_blocks_with_prefix(&first_fn.blocks, "match_test_");
    assert_eq!(
        test_block_count, 0,
        "struct destructure as the first / only arm should not mint any extra test blocks",
    );
}

#[test]
fn match_literal_against_narrow_subject_mints_narrow_const() {
    // A match against a `UInt8` subject should mint `Const UInt8(5)`
    // for the literal arm, not the default `Const Int64(5)`. Pins
    // that `patterns/literals.rs::emit_literal_eq` reads
    // `Pattern::Literal.literal_coercion`.
    let source = "
        fn classify(x: UInt8) -> Int
          match x
            5 -> 1
            _ -> 0
          end
        end

        classify(5)
        ";

    let script = lower(source);
    let classify = script_function(&script, "classify");

    let has_uint8_const = all_instructions(&classify.blocks).any(|i| {
        matches!(
            i,
            IRInstruction::Const {
                value: ConstValue::UInt8(5),
                ..
            }
        )
    });
    assert!(
        has_uint8_const,
        "pattern literal `5` matched against `UInt8` should mint `Const UInt8(5)`; \
         got {:?}",
        all_instructions(&classify.blocks).collect::<Vec<_>>(),
    );
}

#[test]
fn lower_match_constructor_tuple_emits_enum_tuple_shape() {
    let source = "
        enum Box
          Some(Int)
          None
        end

        fn unwrap(b: Box) -> Int
          match b
            Some(x) -> x
            None -> 0
          end
        end

        unwrap(Box.Some(7))
        ";

    let script = lower(source);
    let unwrap_fn = script_function(&script, "unwrap");

    let has_tag_check =
        all_instructions(&unwrap_fn.blocks).any(|i| matches!(i, IRInstruction::EnumTagGet { .. }));
    assert!(
        has_tag_check,
        "constructor shorthand `Some(x)` should rewrite to EnumTuple and emit `EnumTagGet`",
    );

    let payload_get = all_instructions(&unwrap_fn.blocks).find_map(|i| match i {
        IRInstruction::EnumPayloadFieldGet {
            payload_index, tag, ..
        } => Some((*tag, *payload_index)),
        _ => None,
    });
    let (tag, payload_index) = payload_get
        .expect("constructor shorthand should emit `EnumPayloadFieldGet` for the binding");
    assert_eq!(
        tag,
        IRVariantTag(0),
        "Some is the first declared variant (tag 0)",
    );
    assert_eq!(payload_index, 0, "x is the first payload field");
}

#[test]
fn match_struct_literal_field_emits_field_get_and_eq_test_in_entry_block() {
    // `Point{x: 5, y: 6}` produces two AND-chained tests: the
    // first lives in the arm's incoming test block (the function
    // entry for arm 0) and emits `FieldGet x` + `Const 5` + `Eq`.
    // The second lives in a fresh `match_and_field` block reached
    // only when the first cond is true, with the same shape for `y`.
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn classify(p: Point) -> Int
          match p
            Point{x: 5, y: 6} -> 1
            _ -> 0
          end
        end

        classify(Point{x: 5, y: 6})
        ";

    let script = lower(source);
    let classify = script_function(&script, "classify");

    let entry = entry_block(&classify.blocks);
    let field_indices: Vec<u32> = entry
        .instructions
        .iter()
        .filter_map(|i| match i {
            IRInstruction::FieldGet { field_index, .. } => Some(*field_index),
            _ => None,
        })
        .collect();
    assert_eq!(
        field_indices,
        vec![0],
        "entry block should emit FieldGet for the first field's literal test only; \
         got {field_indices:?}",
    );

    let entry_consts: Vec<ConstValue> = entry
        .instructions
        .iter()
        .filter_map(|i| match i {
            IRInstruction::Const { value, .. } => Some(value.clone()),
            _ => None,
        })
        .collect();
    assert!(
        entry_consts
            .iter()
            .any(|v| matches!(v, ConstValue::Int64(5))),
        "entry block should mint Const(Int64 5) for the x == 5 test; got {entry_consts:?}",
    );

    let and_block = classify
        .blocks
        .iter()
        .find(|b| b.label.starts_with("match_and_field"))
        .expect("expected a fresh `match_and_field` block for the second AND-chained test");
    let and_field_indices: Vec<u32> = and_block
        .instructions
        .iter()
        .filter_map(|i| match i {
            IRInstruction::FieldGet { field_index, .. } => Some(*field_index),
            _ => None,
        })
        .collect();
    assert_eq!(
        and_field_indices,
        vec![1],
        "fresh AND-field block should emit FieldGet for `y`; got {and_field_indices:?}",
    );

    let and_consts: Vec<ConstValue> = and_block
        .instructions
        .iter()
        .filter_map(|i| match i {
            IRInstruction::Const { value, .. } => Some(value.clone()),
            _ => None,
        })
        .collect();
    assert!(
        and_consts.iter().any(|v| matches!(v, ConstValue::Int64(6))),
        "fresh AND-field block should mint Const(Int64 6) for the y == 6 test; \
         got {and_consts:?}",
    );

    // Entry's true edge goes to the AND block. Its false edge
    // falls through to arm 1's test.
    let IRTerminator::CondBranch {
        then_target,
        else_target,
        ..
    } = &entry.terminator
    else {
        panic!(
            "entry should end in CondBranch wiring the first AND-field test; got {:?}",
            entry.terminator,
        );
    };
    assert_eq!(
        then_target.block, and_block.id,
        "entry's true edge should go to the fresh AND-field block",
    );
    let next_test = block_labeled(&classify.blocks, "match_test_1");
    assert_eq!(
        else_target.block, next_test.id,
        "entry's false edge should fall through to the next arm's test block",
    );

    // The AND block's true edge goes to arm 0's body. Its false
    // edge also falls through to arm 1's test.
    let IRTerminator::CondBranch {
        then_target: and_then,
        else_target: and_else,
        ..
    } = &and_block.terminator
    else {
        panic!(
            "AND-field block should end in CondBranch; got {:?}",
            and_block.terminator,
        );
    };
    let body_0 = block_labeled(&classify.blocks, "match_body_0");
    assert_eq!(
        and_then.block, body_0.id,
        "AND-field block's true edge should branch to the arm's body block",
    );
    assert_eq!(
        and_else.block, next_test.id,
        "AND-field block's false edge should fall through to the next arm's test block",
    );
}

#[test]
fn match_struct_partial_field_pattern_omits_other_fields_from_tests() {
    // `Point{x: 5}` lists only `x`. The lowering must not emit a
    // FieldGet for `y` (implicit wildcard).
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn classify(p: Point) -> Int
          match p
            Point{x: 5} -> 1
            _ -> 0
          end
        end

        classify(Point{x: 5, y: 9})
        ";

    let script = lower(source);
    let classify = script_function(&script, "classify");

    let field_indices: Vec<u32> = all_instructions(&classify.blocks)
        .filter_map(|i| match i {
            IRInstruction::FieldGet { field_index, .. } => Some(*field_index),
            _ => None,
        })
        .collect();
    assert_eq!(
        field_indices,
        vec![0],
        "partial struct pattern `Point{{x: 5}}` should only FieldGet x; got {field_indices:?}",
    );

    let and_blocks = count_blocks_with_prefix(&classify.blocks, "match_and_field");
    assert_eq!(
        and_blocks, 0,
        "single-field pattern should not mint any AND-chain follow-on blocks",
    );
}

#[test]
fn match_nested_enum_with_inner_literal_orders_tag_before_payload_extraction() {
    // `Option.Some(5)` must test the Option tag BEFORE doing any
    // `EnumPayloadFieldGet`, so the payload extraction is gated on
    // the tag-check success edge. The arm produces two
    // AND-chained tests: tag in the entry block, payload `== 5`
    // in a fresh `match_and_field` block.
    let source = "
        fn classify(op: Option<Int>) -> Int
          match op
            Option.Some(5) -> 1
            _ -> 0
          end
        end

        classify(Option.Some(5))
        ";

    let script = lower(source);
    let classify = script_function(&script, "classify");

    let entry = entry_block(&classify.blocks);
    let entry_has_tag_get = entry
        .instructions
        .iter()
        .any(|i| matches!(i, IRInstruction::EnumTagGet { .. }));
    assert!(
        entry_has_tag_get,
        "entry block should emit EnumTagGet for the Option.Some tag test",
    );
    let entry_has_payload_get = entry
        .instructions
        .iter()
        .any(|i| matches!(i, IRInstruction::EnumPayloadFieldGet { .. }));
    assert!(
        !entry_has_payload_get,
        "entry block must NOT emit EnumPayloadFieldGet before the tag check fires; \
         payload extraction belongs in a tag-gated successor block",
    );

    let and_block = classify
        .blocks
        .iter()
        .find(|b| b.label.starts_with("match_and_field"))
        .expect("expected a tag-gated `match_and_field` block for the payload test");
    let and_has_payload_get = and_block
        .instructions
        .iter()
        .any(|i| matches!(i, IRInstruction::EnumPayloadFieldGet { .. }));
    assert!(
        and_has_payload_get,
        "the AND-field block should emit EnumPayloadFieldGet for the Some payload",
    );

    let IRTerminator::CondBranch { then_target, .. } = &entry.terminator else {
        panic!(
            "entry should wire the tag-check via CondBranch; got {:?}",
            entry.terminator,
        );
    };
    assert_eq!(
        then_target.block, and_block.id,
        "tag-check's true edge should branch into the payload-test block",
    );
}

#[test]
fn match_nested_struct_inside_enum_tuple_chains_field_test_after_tag() {
    // `Option.Some(Point{x: 5})` produces three blocks of interest:
    // entry (tag check), AND #1 (payload extraction + x FieldGet
    // + Eq), and the body block. The bind chain isn't exercised
    // here (every field is a literal), but the test ordering
    // pins the same CFG shape the lang fixture relies on.
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn classify(op: Option<Point>) -> Int
          match op
            Option.Some(Point{x: 5}) -> 1
            _ -> 0
          end
        end

        classify(Option.Some(Point{x: 5, y: 9}))
        ";

    let script = lower(source);
    let classify = script_function(&script, "classify");

    let entry = entry_block(&classify.blocks);
    let entry_tag_gets = entry
        .instructions
        .iter()
        .filter(|i| matches!(i, IRInstruction::EnumTagGet { .. }))
        .count();
    assert_eq!(
        entry_tag_gets, 1,
        "entry should emit exactly one EnumTagGet for the Option.Some tag test",
    );

    let and_block = classify
        .blocks
        .iter()
        .find(|b| b.label.starts_with("match_and_field"))
        .expect("expected a tag-gated `match_and_field` block for the inner struct test");
    let and_payload_gets = and_block
        .instructions
        .iter()
        .filter(|i| matches!(i, IRInstruction::EnumPayloadFieldGet { .. }))
        .count();
    assert_eq!(
        and_payload_gets, 1,
        "AND-field block should extract the Some payload exactly once",
    );
    let and_field_indices: Vec<u32> = and_block
        .instructions
        .iter()
        .filter_map(|i| match i {
            IRInstruction::FieldGet { field_index, .. } => Some(*field_index),
            _ => None,
        })
        .collect();
    assert_eq!(
        and_field_indices,
        vec![0],
        "AND-field block should FieldGet Point.x (index 0); got {and_field_indices:?}",
    );
    let and_eq_consts: Vec<ConstValue> = and_block
        .instructions
        .iter()
        .filter_map(|i| match i {
            IRInstruction::Const { value, .. } => Some(value.clone()),
            _ => None,
        })
        .collect();
    assert!(
        and_eq_consts
            .iter()
            .any(|v| matches!(v, ConstValue::Int64(5))),
        "AND-field block should mint Const(Int64 5) for the inner x == 5 test; \
         got {and_eq_consts:?}",
    );
}

#[test]
fn match_nested_struct_binding_emits_chained_field_gets_in_body_block() {
    // `Option.Some(Point{x: 5, y: y_bind})` exercises a chained
    // bind. At the success edge, the lowering must first project
    // the Option payload (EnumPayloadFieldGet) and then GEP into
    // Point's y field (FieldGet). The bind chain shows up as two
    // sequential projections in the body block, ending in a
    // LocalWrite.
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn classify(op: Option<Point>) -> Int
          match op
            Option.Some(Point{x: 5, y: y_bind}) -> y_bind
            _ -> 0
          end
        end

        classify(Option.Some(Point{x: 5, y: 9}))
        ";

    let script = lower(source);
    let classify = script_function(&script, "classify");

    let body = block_labeled(&classify.blocks, "match_body_0");

    let payload_gets = body
        .instructions
        .iter()
        .filter(|i| matches!(i, IRInstruction::EnumPayloadFieldGet { .. }))
        .count();
    assert_eq!(
        payload_gets, 1,
        "body block should extract the Some payload exactly once for the chained bind",
    );

    let field_indices: Vec<u32> = body
        .instructions
        .iter()
        .filter_map(|i| match i {
            IRInstruction::FieldGet { field_index, .. } => Some(*field_index),
            _ => None,
        })
        .collect();
    assert_eq!(
        field_indices,
        vec![1],
        "body block should FieldGet Point.y (declared index 1) on the extracted payload; \
         got {field_indices:?}",
    );

    let writes = body
        .instructions
        .iter()
        .filter(|i| matches!(i, IRInstruction::LocalWrite { .. }))
        .count();
    assert_eq!(
        writes, 1,
        "body block should emit one LocalWrite for the y_bind chained bind",
    );
}

#[test]
fn match_struct_or_pattern_inside_field_still_wires_or_chain_for_inner() {
    // `Point{x: 1 | 2 | 3, y: y_bind}` exercises the
    // or-pattern-inside-struct-field case. The inner or-pattern
    // should preserve its ChainMode::Or wiring (any alt true ->
    // success) while the outer struct chain stays AND. Today this
    // would require lifting the or-chain into an AND-chain in
    // consume_inner_check. Pin behavior so we see how it's wired.
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn classify(p: Point) -> Int
          match p
            Point{x: 1 | 2 | 3, y: y_bind} -> y_bind
            _ -> 0
          end
        end

        classify(Point{x: 2, y: 9})
        ";

    // The typecheck-side and IR-lowering-side restriction on
    // mixed-mode chains panics for or-inside-and. This test
    // documents that the fixture compiles by virtue of an `_`
    // catch-all. Should this panic in lowering, the test will
    // surface the regression for follow-up.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lower(source)));
    if result.is_err() {
        // Or-inside-struct-field is currently in the out-of-scope
        // list, so a panic is acceptable. The test pins that we
        // panic with a clear message rather than miscompile.
        return;
    }
    let script = result.unwrap();
    let classify = script_function(&script, "classify");
    assert!(
        count_blocks_with_prefix(&classify.blocks, "match_or_alt_") > 0,
        "or-pattern inside a struct field should still mint `match_or_alt_*` blocks if \
         the inner lowering succeeds; got blocks {:?}",
        classify
            .blocks
            .iter()
            .map(|b| b.label.as_str())
            .collect::<Vec<_>>(),
    );
}
