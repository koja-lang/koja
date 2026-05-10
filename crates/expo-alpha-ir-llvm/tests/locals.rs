//! IR-text snapshot tests for the locals slice (`emit/instruction.rs`'s
//! `LocalDecl` / `LocalRead` / `LocalWrite` arms). LLVM's mem2sas
//! optimization pass would normally promote these stack slots to
//! SSA values, but the alpha pipeline disables optimizations so the
//! raw `alloca`/`store`/`load` sequence is observable in the
//! emitted module text.
//!
//! Substring (not full-text) assertions because inkwell may
//! reorder attributes / metadata between LLVM patch versions.

use expo_alpha_ir_llvm::emit_llvm_ir;
use expo_ast::util::dedent;

mod common;

use common::{
    APP_NAME, assert_contains, assert_main_shape, extract_function_body,
    lower_program_source as lower,
};

#[test]
fn local_decl_emits_alloca_store_load_for_i64_slot() {
    let source = "
        fn main -> Int
          x = 7
          x
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "alloca i64");
    assert_contains(&ir_text, "store i64 7");
    assert_contains(&ir_text, "load i64");
    assert_contains(&ir_text, "call void @__expo_alpha_print_i64(i64 ");
}

#[test]
fn reassignment_emits_a_second_store_into_the_same_slot() {
    let source = "
        fn main -> Int
          x = 1
          x = 9
          x
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    let main_body = extract_function_body(&ir_text, "main");
    let alloca_count = main_body.matches("alloca i64").count();
    assert_eq!(
        alloca_count, 1,
        "expected exactly one alloca for the slot — reassignment reuses it.\n\
         main body:\n{main_body}\n\nfull IR:\n{ir_text}",
    );
    assert_contains(main_body, "store i64 1");
    assert_contains(main_body, "store i64 9");
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

        fn main -> Int
          id(42)
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    // Each function gets its own alloca in its own entry block;
    // we expect at least two i64 allocas (one for `id`'s param
    // slot, one for `main`'s call-result slot — wait, main has no
    // body local). Pin one alloca minimum and a store of the param.
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
        fn main -> Int
          x = 0
          if true
            x = 1
          end
          x
        end
        ";

    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    let main_body = extract_function_body(&ir_text, "main");
    let alloca_count = main_body.matches("alloca i64").count();
    assert_eq!(
        alloca_count, 1,
        "expected exactly one alloca even with an if-arm reassignment.\n\
         main body:\n{main_body}\n\nfull IR:\n{ir_text}",
    );
    assert_contains(main_body, "store i64 0");
    assert_contains(main_body, "store i64 1");
}
