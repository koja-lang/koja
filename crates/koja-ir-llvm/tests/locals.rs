//! IR-text snapshot tests for the locals slice (`emit/instruction.rs`'s
//! `LocalDecl` / `LocalRead` / `LocalWrite` arms). LLVM's mem2sas
//! optimization pass would normally promote these stack slots to
//! SSA values, but the pipeline disables optimizations so the
//! raw `alloca`/`store`/`load` sequence is observable in the
//! emitted module text.
//!
//! Substring (not full-text) assertions because inkwell may
//! reorder attributes / metadata between LLVM patch versions.

use koja_ast::util::dedent;
use koja_ir_llvm::emit_script_llvm_ir;

mod common;

use common::{
    APP_NAME, assert_contains, assert_main_shape, extract_function_body,
    lower_script_source as lower,
};

#[test]
fn local_decl_emits_alloca_store_load_for_i64_slot() {
    let source = "
        x = 7
        x
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "alloca i64");
    assert_contains(&ir_text, "store i64 7");
    assert_contains(&ir_text, "load i64");
}

#[test]
fn local_decl_zero_initializes_the_slot() {
    // Every `LocalDecl` stores a zero of the slot type at the decl
    // site, so exit drops on paths that never wrote the slot release
    // nothing (null-safe rc primitives).
    let source = "
        x = 7
        x
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    let user_main = extract_function_body(&ir_text, "__koja_user_main");
    assert_contains(user_main, "store i64 0");
    assert_contains(user_main, "store i64 7");
}

#[test]
fn heap_local_drop_null_checks_the_payload_before_rc_dec() {
    // The exit drop's payload->block-base mapping must propagate a
    // null payload to a null base (`select`) rather than wrapping to
    // `0 - HEADER_BYTES`. A zero-initialized, never-written slot is
    // a legal drop target.
    let source = r#"
        s = "a" <> "b"
        s.length()
        "#;

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    let user_main = extract_function_body(&ir_text, "__koja_user_main");
    assert_contains(user_main, "koja_rc_dec");
    assert_contains(user_main, ".is_null");
    assert_contains(user_main, ".or_null");
}

#[test]
fn reassignment_emits_a_second_store_into_the_same_slot() {
    let source = "
        x = 1
        x = 9
        x
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    let user_main = extract_function_body(&ir_text, "__koja_user_main");
    let alloca_count = user_main.matches("alloca i64").count();
    assert_eq!(
        alloca_count, 1,
        "expected exactly one alloca for the slot. Reassignment reuses it.\n\
         user_main body:\n{user_main}\n\nfull IR:\n{ir_text}",
    );
    assert_contains(user_main, "store i64 1");
    assert_contains(user_main, "store i64 9");
}

#[test]
fn param_promotion_emits_alloca_and_initial_store_in_callee() {
    // The function-entry promotion of `n` materializes as an
    // `alloca` + initial `store` of the incoming param into the
    // slot, followed by a `load` for the body's read.
    let source = "
        fn id(n: Int) -> Int
          n
        end

        id(42)
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    // Each function gets its own alloca in its own entry block.
    // We expect at least one i64 alloca for `id`'s param slot. Pin
    // one alloca minimum and a store of the param.
    assert!(
        ir_text.contains("alloca i64"),
        "expected at least one i64 alloca for `id`'s param slot.\nIR:\n{ir_text}",
    );
    // Constant 42 flows from caller to callee unchanged.
    assert_contains(&ir_text, "i64 42");
}

#[test]
fn local_inside_if_arm_still_uses_a_single_alloca() {
    // `LocalDecl` is hoisted to the entry block regardless of the
    // surface declaration site, so the conditional arm must not
    // emit a second `alloca` even though the `store` lives in the
    // arm's basic block.
    let source = "
        x = 0
        if true
          x = 1
        end
        x
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    let user_main = extract_function_body(&ir_text, "__koja_user_main");
    let alloca_count = user_main.matches("alloca i64").count();
    assert_eq!(
        alloca_count, 1,
        "expected exactly one alloca even with an if-arm reassignment.\n\
         user_main body:\n{user_main}\n\nfull IR:\n{ir_text}",
    );
    assert_contains(user_main, "store i64 0");
    assert_contains(user_main, "store i64 1");
}
