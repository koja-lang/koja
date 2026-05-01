//! Function and method call dispatch (free functions, generic
//! instantiation, `impl`-block methods).
//!
//! Maps to LANGUAGE.md "Functions".

mod common;

use common::{dedent, eval_entry};
use expo_ir_eval::Value;

#[test]
fn evaluates_function_call() {
    let source = "
        fn double(x: Int) -> Int
          2 * x
        end

        fn run -> Int
          double(21)
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(42));
}

#[test]
fn evaluates_generic_function_call() {
    let source = "
        fn identity<T>(x: T) -> T
          x
        end

        fn run -> Int
          identity(42)
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(42));
}

#[test]
fn evaluates_generic_method_call() {
    // Verifies the slice 3 method-closure pass: the call site
    // `p.constant_42()` lifts to `IRInstruction::MethodCall` because
    // the closure pass pre-registers `__test__.Pair_$Int.Int$_constant_42`.
    // Method body intentionally avoids `FieldAccess` (a separate Stub
    // lift) so this test isolates the method-dispatch path.
    let source = "
        struct Pair<L, R>
          left: L
          right: R
        end

        impl Pair<L, R>
          fn constant_42(self) -> Int
            42
          end
        end

        fn run -> Int
          p = Pair { left: 7, right: 9 }
          p.constant_42()
        end
        ";

    let program = dedent(source);
    let value = eval_entry(&program, "run");
    assert_eq!(value, Value::Int(42));
}
