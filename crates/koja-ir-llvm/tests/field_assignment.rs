//! IR-text snapshot tests for the multi-segment field-assignment
//! lowering: `IRInstruction::FieldSet` and the optional
//! `IRInstruction::DropValue` that precedes it on a heap-typed leaf
//! overwrite.
//!
//! Each `FieldSet` lowers to one `insertvalue` of the new value over
//! the base aggregate, which is the instruction's SSA result. A
//! multi-segment write chains one `insertvalue` per depth from the
//! innermost struct out. Assertions are substring-only. Byte-for-byte
//! stdout coverage of the same fixtures lives in the `koja-driver`
//! e2e suite.

use koja_ast::util::dedent;
use koja_ir_llvm::emit_script_llvm_ir;

mod common;

use common::{APP_NAME, assert_contains, extract_function_body, lower_script_source as lower};

#[test]
fn depth_one_field_write_emits_insertvalue_over_the_base() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        p = Point{x: 1, y: 2}
        p.x = 10
        p.x
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    let main_body = extract_function_body(&ir_text, "__koja_user_main");
    assert_contains(main_body, "insertvalue %TestApp.Point");
    assert_contains(main_body, ", i64 10, 0");
}

#[test]
fn depth_two_field_write_emits_one_insertvalue_per_depth() {
    let source = "
        struct Inner
          n: Int
        end

        struct Outer
          inner: Inner
        end

        o = Outer{inner: Inner{n: 1}}
        o.inner.n = 42
        o.inner.n
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    // The literal folds to a constant aggregate, so the only
    // `insertvalue`s in the body are the two FieldSet rebuilds, the
    // inner struct first and then the outer one around it.
    let main_body = extract_function_body(&ir_text, "__koja_user_main");
    let insert_inner = main_body.matches("insertvalue %TestApp.Inner").count();
    let insert_outer = main_body.matches("insertvalue %TestApp.Outer").count();
    assert_eq!(
        insert_inner, 1,
        "expected one `insertvalue %TestApp.Inner` for the inner FieldSet, got \
         {insert_inner}\nIR:\n{main_body}",
    );
    assert_eq!(
        insert_outer, 1,
        "expected one `insertvalue %TestApp.Outer` for the outer FieldSet, got \
         {insert_outer}\nIR:\n{main_body}",
    );
    assert_contains(main_body, ", i64 42, 0");
}

#[test]
fn heap_leaf_overwrite_emits_free_call_for_drop_value() {
    let source = "
        struct Holder
          name: String
        end

        h = Holder{name: \"old\"}
        h.name = \"new\"
        1
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(&ir_text, "call void @koja_free");
}
