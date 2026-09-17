//! IR-text coverage for pooled constants. A pooled enum variant
//! materializes as a true LLVM constant, so it can sit inside a
//! constant struct and be shared by every function that reads it.
//! Both shapes failed module verification before the constant
//! emitter stopped building enum values through an alloca.

use koja_ast::util::dedent;
use koja_ir_llvm::{emit_llvm_ir, emit_script_llvm_ir};

mod common;

use common::{APP_NAME, assert_contains, lower_program_source, lower_script_source};

#[test]
fn enum_constant_read_from_two_functions_is_one_constant() {
    let source = "
        enum Color
          Red
          Green
        end

        const DEFAULT = Color.Green

        fn a() -> Color
          DEFAULT
        end

        fn b() -> Color
          DEFAULT
        end

        fn main() -> Int
          match a()
            Color.Green -> match b()
              Color.Green -> 0
              _ -> 1
            end
            _ -> 1
          end
        end
        ";

    let program = lower_program_source(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_contains(&ir_text, "ret %TestApp.Color { [1 x i8] c\"\\01\" }");
}

#[test]
fn struct_constant_with_an_enum_field_is_a_constant_aggregate() {
    let source = "
        enum Unit
          Seconds
          Minutes
        end

        struct Span
          value: Int
          unit: Unit
        end

        const ONE_MINUTE = Span{value: 1, unit: Unit.Minutes}

        ONE_MINUTE.value
        ";

    let script = lower_script_source(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(
        &ir_text,
        "%TestApp.Span { i64 1, %TestApp.Unit { [1 x i8] c\"\\01\" } }",
    );
}

#[test]
fn nested_constant_lowers_like_a_package_constant() {
    let source = "
        struct Span
          value: Int
          unit: Span.Unit

          enum Unit
            Seconds
            Minutes
          end

          const ZERO = Span{value: 0, unit: Span.Unit.Minutes}
        end

        struct Summary
          elapsed: Span = Span.ZERO
        end

        Summary{}.elapsed.value + Span.ZERO.value
        ";

    let script = lower_script_source(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(
        &ir_text,
        "%TestApp.Span { i64 0, %TestApp.Span.Unit { [1 x i8] c\"\\01\" } }",
    );
}
