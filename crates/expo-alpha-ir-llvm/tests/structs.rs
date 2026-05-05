//! IR-text snapshot tests for the struct slice in
//! [`expo_alpha_ir_llvm::emit_script_llvm_ir`].
//!
//! Three contracts are pinned:
//!
//! - **Pre-emission types**: every `IRStructDecl` becomes a named LLVM
//!   `%Pkg.Name = type { ... }` with the field list translated by
//!   [`crate::types::ir_basic_type`]. Mutually-referential types
//!   resolve through the two-phase `declare → set_body` loop in
//!   [`crate::emit::structs`].
//! - **`StructInit` lowering**: a `Type{...}` literal lowers to
//!   `alloca → store-per-field via getelementptr → load`. The alloca
//!   lives in the function's entry block (per
//!   [`EmitCtx::build_entry_alloca`]) so the canonicalized field-init
//!   order in the IR layer flows straight into linear `store`
//!   sequences in the bitcode.
//! - **`FieldGet` lowering**: a `recv.field` projection lowers to
//!   alloca → store-receiver → `getelementptr inbounds → load`,
//!   mirroring v1 codegen's `emit_field_load` shape.
//!
//! All assertions are substring-only (LLVM may shuffle attribute
//! ordering between patch versions). Byte-for-byte stdout coverage of
//! the same fixtures lives in the `expo-driver` e2e suite.

use expo_alpha_ir_llvm::emit_script_llvm_ir;
use expo_ast::util::dedent;

mod common;

use common::{APP_NAME, assert_contains, assert_main_shape, lower_script_source as lower};

#[test]
fn struct_decl_emits_named_llvm_struct_type() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        Point{x: 5, y: 10}.x
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "%TestApp.Point = type { i64, i64 }");
}

#[test]
fn struct_init_lowers_to_alloca_store_per_field_then_load() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        Point{x: 5, y: 10}.x
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(&ir_text, "alloca %TestApp.Point");
    assert_contains(&ir_text, "getelementptr inbounds %TestApp.Point");
    assert_contains(&ir_text, "store i64 5");
    assert_contains(&ir_text, "store i64 10");
}

#[test]
fn field_get_lowers_to_gep_then_load() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        Point{x: 5, y: 10}.x
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    // FieldGet always projects through alloca → store-receiver →
    // GEP → load (mirroring v1 codegen's `emit_field_load`). The
    // load result is passed to the auto-print runtime by register,
    // so we don't pin the literal `5` at the print call site —
    // that would over-constrain the assertion to a particular SSA
    // numbering inkwell happens to assign today.
    assert_contains(&ir_text, "getelementptr inbounds %TestApp.Point");
    assert_contains(&ir_text, "load i64");
    assert_contains(&ir_text, "call void @__expo_alpha_print_i64");
}

#[test]
fn struct_with_mixed_field_types_emits_each_llvm_type() {
    let source = "
        struct Profile
          age: Int
          active: Bool
        end

        Profile{age: 30, active: true}.age
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    // Bool lowers to i1 in the alpha transient set; the
    // transient-set rule lives in `seal::require_supported_type`.
    assert_contains(&ir_text, "%TestApp.Profile = type { i64, i1 }");
    assert_contains(&ir_text, "store i64 30");
    assert_contains(&ir_text, "store i1 true");
    assert_contains(&ir_text, "call void @__expo_alpha_print_i64");
}

#[test]
fn nested_struct_emits_inner_type_inside_outer_field_layout() {
    let source = "
        struct Inner
          n: Int
        end

        struct Outer
          inner: Inner
          tag: Bool
        end

        Outer{inner: Inner{n: 42}, tag: false}.inner.n
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "%TestApp.Inner = type { i64 }");
    assert_contains(&ir_text, "%TestApp.Outer = type { %TestApp.Inner, i1 }");
    // Cross-struct field reference: Outer's first slot is the
    // *Inner struct value*, not a flat i64. Pinning the named-type
    // body confirms the two-phase pre-emit (`declare → set_body`)
    // resolves the inner type by symbol when sizing Outer's field
    // list.
    assert_contains(&ir_text, "store %TestApp.Inner");
    assert_contains(&ir_text, "call void @__expo_alpha_print_i64");
}
