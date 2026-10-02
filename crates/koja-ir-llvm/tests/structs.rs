//! IR-text snapshot tests for the struct slice in
//! [`koja_ir_llvm::emit_script_llvm_ir`].
//!
//! Three contracts are pinned:
//!
//! - **Pre-emission types**: every `IRStructDecl` becomes a named LLVM
//!   `%Pkg.Name = type { ... }` with the field list translated by
//!   [`crate::types::ir_basic_type`]. Mutually-referential types
//!   resolve through the two-phase `declare -> set_body` loop in
//!   [`crate::layout::structs`].
//! - **`StructInit` lowering**: a `Type{...}` literal lowers to one
//!   `insertvalue` per field over `undef`, in the canonicalized
//!   field-init order from the IR layer.
//! - **`FieldGet` lowering**: a `recv.field` projection lowers to one
//!   `extractvalue` on the receiver aggregate.
//!
//! LLVM folds `insertvalue` / `extractvalue` over constant operands,
//! so a literal with constant fields becomes a constant aggregate and
//! a projection out of it becomes the field constant. The shape tests
//! therefore route the fields through function parameters.
//!
//! All assertions are substring-only (LLVM may shuffle attribute
//! ordering between patch versions). Byte-for-byte stdout coverage of
//! the same fixtures lives in the `koja-driver` e2e suite.

use koja_ast::util::dedent;
use koja_ir_llvm::emit_script_llvm_ir;

mod common;

use common::{
    APP_NAME, assert_contains, assert_main_shape, extract_function_body,
    lower_script_source as lower,
};

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
fn struct_init_lowers_to_insertvalue_per_field() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn make(a: Int, b: Int) -> Point
          Point{x: a, y: b}
        end

        make(5, 10).x
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    let make_body = extract_function_body(&ir_text, "TestApp.make");
    assert_contains(make_body, "insertvalue %TestApp.Point undef, i64");
    assert_contains(
        make_body,
        "insertvalue %TestApp.Point %TestApp.Point_field_0, i64",
    );
}

#[test]
fn field_get_lowers_to_extractvalue() {
    let source = "
        struct Point
          x: Int
          y: Int

          fn first(self) -> Int
            self.x
          end
        end

        Point{x: 5, y: 10}.first()
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    let first_body = extract_function_body(&ir_text, "TestApp.Point.first");
    assert_contains(first_body, "%field_0 = extractvalue %TestApp.Point");
}

#[test]
fn struct_with_mixed_field_types_emits_each_llvm_type() {
    let source = "
        struct Profile
          age: Int
          active: Bool
        end

        fn make(age: Int, active: Bool) -> Profile
          Profile{age: age, active: active}
        end

        make(30, true).age
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    // Bool lowers to i1 in the transient set. The
    // transient-set rule lives in `seal::require_supported_type`.
    assert_contains(&ir_text, "%TestApp.Profile = type { i64, i1 }");
    let make_body = extract_function_body(&ir_text, "TestApp.make");
    assert_contains(make_body, "insertvalue %TestApp.Profile undef, i64");
    assert_contains(
        make_body,
        "insertvalue %TestApp.Profile %TestApp.Profile_field_0, i1",
    );
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
    // body confirms the two-phase pre-emit (`declare -> set_body`)
    // resolves the inner type by symbol when sizing Outer's field
    // list.
    assert_contains(&ir_text, "store %TestApp.Inner");
}

#[test]
fn inline_static_method_emits_named_function_definition() {
    // Inline-form static method: a `define %TestApp.Point @TestApp.Point.origin()`
    // function should appear alongside the `main` wrapper, with the
    // call site dispatching to it by mangled symbol.
    let source = "
        struct Point
          x: Int
          y: Int

          fn origin -> Point
            Point{x: 0, y: 0}
          end
        end

        Point.origin().x
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(
        &ir_text,
        "define %TestApp.Point @\"TestApp.Point.origin/0\"()",
    );
    assert_contains(
        &ir_text,
        "call %TestApp.Point @\"TestApp.Point.origin/0\"()",
    );
}

#[test]
fn impl_block_static_method_emits_named_function_definition() {
    // Impl-form mirror of the inline test: same expected emit
    // because both surface forms register under the same qualified
    // identifier and lower to the same `IRSymbol`.
    let source = "
        struct Point
          x: Int
          y: Int
        end

        extend Point
          fn origin -> Point
            Point{x: 0, y: 0}
          end
        end

        Point.origin().x
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(
        &ir_text,
        "define %TestApp.Point @\"TestApp.Point.origin/0\"()",
    );
    assert_contains(
        &ir_text,
        "call %TestApp.Point @\"TestApp.Point.origin/0\"()",
    );
}

#[test]
fn static_method_with_args_emits_typed_signature_and_call() {
    let source = "
        struct Point
          x: Int

          fn at(seed: Int, _scale: Int) -> Int
            42
          end
        end

        Point.at(7, 3)
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "define i64 @\"TestApp.Point.at/2\"(i64");
    assert_contains(&ir_text, "call i64 @\"TestApp.Point.at/2\"(i64 7, i64 3)");
}

#[test]
fn inline_instance_method_emits_named_function_with_self_param() {
    let source = "
        struct Point
          x: Int
          y: Int

          fn first(self) -> Int
            self.x
          end
        end

        Point{x: 7, y: 3}.first()
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    // The signature carries the receiver-by-value as the first
    // parameter. Inkwell emits `%TestApp.Point` for the type.
    assert_contains(
        &ir_text,
        "define i64 @\"TestApp.Point.first/1\"(%TestApp.Point",
    );
    // Call site threads the receiver value as the first arg.
    assert_contains(
        &ir_text,
        "call i64 @\"TestApp.Point.first/1\"(%TestApp.Point",
    );
}

#[test]
fn impl_block_instance_method_emits_named_function_with_self_param() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        extend Point
          fn second(self) -> Int
            self.y
          end
        end

        Point{x: 7, y: 3}.second()
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(
        &ir_text,
        "define i64 @\"TestApp.Point.second/1\"(%TestApp.Point",
    );
    assert_contains(
        &ir_text,
        "call i64 @\"TestApp.Point.second/1\"(%TestApp.Point",
    );
}

#[test]
fn instance_method_with_explicit_arg_emits_signature_and_call() {
    let source = "
        struct Counter
          n: Int

          fn add(self, delta: Int) -> Int
            self.n + delta
          end
        end

        Counter{n: 10}.add(5)
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    // Signature: `(self: %TestApp.Counter, delta: i64) -> i64`.
    assert_contains(
        &ir_text,
        "define i64 @\"TestApp.Counter.add/2\"(%TestApp.Counter",
    );
    // Call site threads the receiver value first, then the explicit
    // `5`. Inkwell emits the receiver as a register reference, so
    // pin the literal-arg suffix instead.
    assert_contains(&ir_text, ", i64 5)");
}
