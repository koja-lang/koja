//! IR-text coverage for pooled constants. A pooled enum variant
//! materializes as a true LLVM constant, so it can sit inside a
//! constant struct and be shared by every function that reads it.
//! Both shapes failed module verification before the constant
//! emitter stopped building enum values through an alloca. A built
//! constant instead gets one internal global that `__koja_const_init`
//! fills from PID 1's entry code before any user code runs.

use koja_ast::util::dedent;
use koja_ir_llvm::{emit_llvm_ir, emit_script_llvm_ir};

mod common;

use common::{
    APP_NAME, assert_contains, extract_function_body, lower_program_source, lower_script_source,
};

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

#[test]
fn built_constant_has_one_global_one_init_call_and_a_load_per_read() {
    let source = "
        const PRIMES = [2, 3, 5]

        fn a() -> Int
          PRIMES.length()
        end

        fn b() -> Int
          PRIMES.length()
        end

        fn main() -> Int
          a() + b()
        end
        ";

    let program = lower_program_source(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    // A `List` is a `{ ptr, i64, i64 }` value in this backend.
    assert_contains(
        &ir_text,
        "@koja_const.TestApp.PRIMES = internal global { ptr, i64, i64 } zeroinitializer",
    );
    assert_eq!(
        ir_text.matches("@koja_const.TestApp.PRIMES = ").count(),
        1,
        "one global per built constant",
    );
    // No static constructor. The init runs inside PID 1, called once
    // from the process entry wrapper and nowhere else.
    assert!(
        !ir_text.contains("llvm.global_ctors"),
        "built constants must not register a static constructor:\n{ir_text}",
    );
    assert_contains(&ir_text, "define internal void @__koja_const_init()");
    let entry_wrapper = extract_function_body(&ir_text, "TestApp.TestEntry.__entry_wrapper");
    assert_contains(entry_wrapper, "call void @__koja_const_init()");
    assert_eq!(
        ir_text.matches("call void @__koja_const_init()").count(),
        1,
        "only the entry wrapper calls the init",
    );
    let init = extract_function_body(&ir_text, "__koja_const_init");
    assert!(
        init.contains("call { ptr, i64, i64 } @TestApp.PRIMES__init()")
            && init.contains("store { ptr, i64, i64 } %built, ptr @koja_const.TestApp.PRIMES"),
        "the init should call each constant's init and store its result:\n{init}",
    );
    assert_eq!(
        ir_text
            .matches("load { ptr, i64, i64 }, ptr @koja_const.TestApp.PRIMES")
            .count(),
        2,
        "each read loads the global",
    );
}

#[test]
fn init_calls_inits_in_built_constant_order_from_user_main() {
    // `ALL` sorts before `BASE` but reads it, so the IR order puts
    // `BASE` first and `__koja_const_init` must follow that order.
    // The script thunk that runs as PID 1 calls it once.
    let source = "
        const ALL = [BASE]
        const BASE = [1, 2]

        ALL.length()
        ";

    let script = lower_script_source(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    let names: Vec<&str> = script
        .built_constant_order
        .iter()
        .map(|symbol| symbol.mangled())
        .collect();
    assert_eq!(names, ["TestApp.BASE", "TestApp.ALL"]);
    let init = extract_function_body(&ir_text, "__koja_const_init");
    let base = init
        .find("@TestApp.BASE__init()")
        .expect("init calls BASE init");
    let all = init
        .find("@TestApp.ALL__init()")
        .expect("init calls ALL init");
    assert!(base < all, "BASE must build before ALL:\n{init}");

    let user_main = extract_function_body(&ir_text, "__koja_user_main");
    assert_contains(user_main, "call void @__koja_const_init()");
    assert_eq!(
        ir_text.matches("call void @__koja_const_init()").count(),
        1,
        "only the user-main thunk calls the init",
    );
}
