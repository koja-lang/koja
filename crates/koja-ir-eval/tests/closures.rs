//! Coverage for closure runtime in the eval interpreter.
//!
//! Pins runtime behavior across the two dispatch shapes the lower
//! pass emits — direct user closures and named-fn-as-value adapters
//! — including capture lifetimes, env indexing through
//! [`koja_ir::IRInstruction::LoadCapture`], higher-order
//! parameter passing, and heap-typed captures whose outer slot is
//! moved into the env.

use koja_ast::util::dedent;
use koja_ir_eval::Value;

mod common;

use common::evaluate_script as evaluate;

#[test]
fn captureless_block_closure_invokes_through_local_call() {
    let source = "
        f = fn (x: Int) -> Int
          x + 1
        end
        f(41)
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(42));
}

#[test]
fn block_closure_with_int_capture_reads_through_load_capture() {
    let source = "
        y = 10
        f = fn (x: Int) -> Int
          x + y
        end
        f(5)
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(15));
}

#[test]
fn closure_capturing_two_locals_indexes_env_in_declaration_order() {
    let source = "
        a = 100
        b = 20
        f = fn (x: Int) -> Int
          a + b + x
        end
        f(3)
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(123));
}

#[test]
fn closure_invoked_twice_reuses_environment() {
    let source = "
        y = 7
        f = fn (x: Int) -> Int
          x + y
        end
        f(1) + f(2)
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(17));
}

#[test]
fn heap_typed_capture_routes_through_env_and_runs_inside_body() {
    // The closure captures heap-typed `s`, copied into the env via a
    // LocalRead of the outer slot (value semantics). The body reads
    // `s` via LoadCapture and passes it to `length`, exercising the
    // env-allocation + capture-lookup contract end to end without
    // needing a real intrinsic.
    let source = "
        fn length(_s: String) -> Int
          3
        end

        s = \"hi\" <> \"there\"
        f = fn (n: Int) -> Int
          length(s) + n
        end
        f(10)
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(13));
}

#[test]
fn heap_capture_is_independent_and_survives_repeated_invocation() {
    // The capture-acquire lowering clones the heap-typed `s` into the
    // env, so the env owns its own copy: invoking the closure twice
    // and using `s` again afterward must all see the same value
    // (eval reclaims via its host GC, but the value-semantics shape
    // must match the LLVM backend's rc path).
    let source = "
        fn length(_s: String) -> Int
          5
        end

        s = \"hello\"
        f = fn (n: Int) -> Int
          length(s) + n
        end
        a = f(1)
        b = f(2)
        a + b + length(s)
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(18));
}

#[test]
fn fn_as_value_adapter_dispatches_through_make_closure() {
    let source = "
        fn add(x: Int, y: Int) -> Int
          x + y
        end

        fn apply(f: fn (Int, Int) -> Int, x: Int, y: Int) -> Int
          f(x, y)
        end

        apply(add, 40, 2)
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(42));
}

#[test]
fn higher_order_function_invokes_user_closure_through_param_slot() {
    let source = "
        fn apply(f: fn (Int) -> Int, x: Int) -> Int
          f(x)
        end

        y = 10
        g = fn (x: Int) -> Int
          x + y
        end
        apply(g, 5)
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(15));
}

#[test]
fn short_closure_form_runs_with_capture() {
    let source = "
        fn apply(f: fn (Int) -> Int, x: Int) -> Int
          f(x)
        end

        y = 3
        apply(x -> x * y, 14)
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(42));
}

#[test]
fn closure_value_renders_through_display() {
    // A closure can be returned from a function; its Display impl
    // must surface a recognizable shape so `--auto-print` produces
    // human-readable stdout for closure-typed mains. Captures render
    // inline; the body symbol stays mangled (matches the LLVM
    // backend's expected stdout).
    let source = "
        y = 7
        fn (x: Int) -> Int
          x + y
        end
        ";
    let value = evaluate(&dedent(source)).unwrap();
    let rendered = format!("{value}");
    assert!(
        rendered.starts_with("<closure "),
        "closure Display should be `<closure ...>`, got `{rendered}`",
    );
    assert!(
        rendered.contains("env=[7]"),
        "single Int capture should render as `env=[7]`, got `{rendered}`",
    );
}
