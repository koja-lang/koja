//! IR-text snapshot tests for the multi-segment field-assignment
//! lowering — `IRInstruction::FieldSet` and the optional
//! `IRInstruction::DropValue` that precedes it on a heap-typed leaf
//! overwrite.
//!
//! Each `FieldSet` lowers to the same alloca + per-field store + load
//! shape as `StructInit` (one alloca per `FieldSet`, one store of the
//! new value into the GEP'd slot, one load to materialize the rebuilt
//! struct as the instruction's SSA result). Assertions are substring-
//! only; byte-for-byte stdout coverage of the same fixtures lives in
//! the `expo-driver` e2e suite.

use expo_alpha_ir_llvm::emit_script_llvm_ir;
use expo_ast::util::dedent;

mod common;

use common::{APP_NAME, assert_contains, lower_script_source as lower};

#[test]
fn depth_one_field_write_emits_alloca_gep_store_load_shape() {
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

    assert_contains(&ir_text, "alloca %TestApp.Point");
    assert_contains(&ir_text, "getelementptr inbounds %TestApp.Point");
    assert_contains(&ir_text, "store i64 10");
}

#[test]
fn depth_two_field_write_emits_two_field_set_alloca_blocks() {
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

    let alloca_outer = ir_text.matches("alloca %TestApp.Outer").count();
    let alloca_inner = ir_text.matches("alloca %TestApp.Inner").count();
    assert!(
        alloca_outer >= 2,
        "expected at least two `alloca %TestApp.Outer` (one for the struct literal and one \
         for FieldSet's rebuild), got {alloca_outer}\nIR:\n{ir_text}",
    );
    assert!(
        alloca_inner >= 2,
        "expected at least two `alloca %TestApp.Inner` (one for the struct literal and one \
         for FieldSet's rebuild), got {alloca_inner}\nIR:\n{ir_text}",
    );
    assert_contains(&ir_text, "store i64 42");
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

    assert_contains(&ir_text, "call void @free");
}
