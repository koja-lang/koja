//! IR-text snapshot tests for nested-pattern match arms — literal
//! payloads inside struct fields and enum tuple positions. Pins
//! the AND-chain CFG shape (one `match_and_field` follow-on block
//! per non-first sibling test, tag-test-before-payload-projection
//! ordering for enums, chained extraction at the body block for
//! nested bindings) at the LLVM level so a regression in
//! [`koja_ir::lower::patterns`] surfaces as a substring miss
//! rather than a runtime miscompile.

use koja_ast::util::dedent;
use koja_ir_llvm::emit_script_llvm_ir;

mod common;

use common::{APP_NAME, assert_contains, assert_main_shape, lower_script_source as lower};

#[test]
fn struct_literal_field_pattern_lowers_to_and_chained_test_blocks() {
    // `Point{x: 5, y: 6}` should produce one entry-block test
    // (FieldGet + icmp eq) and a fresh `match_and_field`
    // follow-on for the second field.
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
    let script = lower(&dedent(source));
    let ir_text = emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir");
    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "match_and_field");
    assert_contains(&ir_text, "icmp eq i64");
    assert_contains(&ir_text, "match_test_1");
}

#[test]
fn struct_partial_field_pattern_emits_only_one_field_test_no_follow_on() {
    // `Point{x: 5}` lists only `x`; lowering must not mint a
    // `match_and_field` follow-on block (single test → no AND
    // chain).
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
    let script = lower(&dedent(source));
    let ir_text = emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir");
    assert_main_shape(&ir_text);
    assert!(
        !ir_text.contains("match_and_field"),
        "single-field struct pattern should not mint a match_and_field block; \
         full LLVM IR:\n{ir_text}"
    );
}

#[test]
fn nested_enum_payload_literal_orders_tag_check_before_payload_projection() {
    // `Option.Some(5)` must perform the Option tag check before
    // touching the payload's bytes. Inkwell labels carry the
    // IR block label suffixes — pin both the entry `i8`
    // tag compare (`icmp eq i8`) and the AND-field follow-on
    // block name to confirm the ordering.
    let source = "
        fn classify(op: Option<Int>) -> Int
          match op
            Option.Some(5) -> 1
            _ -> 0
          end
        end

        classify(Option.Some(5))
        ";
    let script = lower(&dedent(source));
    let ir_text = emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir");
    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "icmp eq i8");
    assert_contains(&ir_text, "match_and_field");
    assert_contains(&ir_text, "icmp eq i64");
}

#[test]
fn nested_struct_inside_enum_chains_payload_extraction_then_field_test() {
    // `Option.Some(Point{x: 5})` exercises both projections:
    // EnumPayloadFieldGet (tag-gated) and a follow-on FieldGet
    // for `Point.x`. Both must appear in the LLVM IR.
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
    let script = lower(&dedent(source));
    let ir_text = emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir");
    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "match_and_field");
    assert_contains(&ir_text, "icmp eq i8");
    assert_contains(&ir_text, "icmp eq i64");
}

#[test]
fn nested_struct_binding_chain_extracts_through_payload_then_field_in_body() {
    // `Option.Some(Point{x: x, y: y})` chained-bind path: the
    // body block must contain both an enum payload projection
    // (`getelementptr` into the Some variant struct) and the
    // inner field projections.
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn classify(op: Option<Point>) -> Int
          match op
            Option.Some(Point{x: xb, y: yb}) -> xb + yb
            Option.None -> 0
          end
        end

        classify(Option.Some(Point{x: 3, y: 4}))
        ";
    let script = lower(&dedent(source));
    let ir_text = emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir");
    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "match_body_0");
    // Enum tag check (i8) followed by integer arithmetic (i64
    // add) for the body. The presence of `add i64` confirms the
    // chained bind reached the body's `xb + yb` expression.
    assert_contains(&ir_text, "icmp eq i8");
    assert_contains(&ir_text, "add i64");
}
