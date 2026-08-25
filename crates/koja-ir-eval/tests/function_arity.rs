use koja_ir_eval::Value;

mod common;

fn evaluate(source: &str) -> Value {
    common::evaluate_script(source).expect("evaluation should succeed")
}

#[test]
fn calls_distinct_function_and_method_overloads() {
    let value = evaluate(
        "
        fn choose(value: Int) -> Int
          value
        end

        fn choose(left: Int, right: Int) -> Int
          left + right
        end

        struct Counter
          value: Int

          fn add(self, value: Int) -> Int
            self.value + value
          end

          fn add(self, left: Int, right: Int) -> Int
            self.value + left + right
          end
        end

        counter = Counter{value: 10}
        choose(1) + choose(2, 3) + counter.add(4) + counter.add(5, 6)
        ",
    );
    assert_eq!(value, Value::Int(41));
}

#[test]
fn calls_default_adapter_through_named_reference() {
    let value = evaluate(
        "
        fn add(left: Int, right: Int = 4) -> Int
          left + right
        end

        apply = &add/1
        apply(3)
        ",
    );
    assert_eq!(value, Value::Int(7));
}

/// A fallible function's default adapter used to forward the
/// canonical call's `Result` into its own Ok-autowrap and fail to
/// typecheck. The adapter now unwraps with `try` before rewrapping.
#[test]
fn fallible_default_adapter_forwards_through_try() {
    let value = evaluate(
        "
        fn half(x: Int, y: Int = 2) -> Int ! String
          if y == 0
            fail \"zero\"
          end

          x / y
        end

        match half(10)
          Result.Ok(n) -> n
          Result.Err(_) -> -1
        end
        ",
    );
    assert_eq!(value, Value::Int(5));
}

#[test]
fn generic_overloads_monomorphize_independently() {
    let value = evaluate(
        "
        fn identity<T>(value: T) -> T
          value
        end

        fn identity<T>(first: T, second: T) -> T
          second
        end

        identity(4) + identity(5, 6)
        ",
    );
    assert_eq!(value, Value::Int(10));
}
