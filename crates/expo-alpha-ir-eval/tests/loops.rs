//! Coverage for `while` execution in the eval interpreter.
//!
//! Pins runtime behavior: counter accumulator, loop-carried heap
//! state (string concat in body), early `return` from inside loop,
//! and the trailing-expression Unit shape that the surface
//! expression takes on.

use expo_alpha_ir_eval::Value;
use expo_ast::util::dedent;

mod common;

use common::evaluate_program as evaluate;

#[test]
fn while_counter_accumulator_sums_first_ten_integers() {
    let source = "
        fn main -> Int
          i = 0
          sum = 0
          while i < 10
            sum = sum + i
            i = i + 1
          end
          sum
        end
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(45));
}

#[test]
fn while_with_zero_iterations_returns_initial_value() {
    let source = "
        fn main -> Int
          i = 0
          while i > 0
            i = i + 1
          end
          i
        end
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(0));
}

#[test]
fn while_with_string_concat_in_body_accumulates_heap_state() {
    // Loop-carried heap-typed slot. Each iteration's `s = s <> "x"`
    // reassignment drops the prior value via the existing
    // ownership-aware `LocalWrite` drop on reassignment.
    let source = "
        fn main -> String
          i = 0
          s = \"\"
          while i < 3
            s = s <> \"x\"
            i = i + 1
          end
          s
        end
        ";
    assert_eq!(
        evaluate(&dedent(source)).unwrap(),
        Value::String("xxx".into()),
    );
}

#[test]
fn early_return_inside_while_exits_function() {
    let source = "
        fn main -> Int
          i = 0
          while i < 100
            i = i + 1
            if i == 5
              return i
            end
          end
          0
        end
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(5));
}

#[test]
fn while_value_is_unit() {
    // A trailing `while` produces Unit (loops type as Unit in alpha,
    // mirroring v1).
    let source = "
        fn main
          i = 0
          while i < 1
            i = i + 1
          end
        end
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Unit);
}

#[test]
fn nested_while_loops_iterate_correctly() {
    let source = "
        fn main -> Int
          i = 0
          total = 0
          while i < 3
            j = 0
            while j < 4
              total = total + 1
              j = j + 1
            end
            i = i + 1
          end
          total
        end
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(12));
}

/// `Enumeration<Int>` fixture for the `for` tests below. `get`
/// always returns `Some(...)` — the desugar's `__idx < __len`
/// guard ensures it's only called for valid indices, and a
/// literal `None` branch needs return-type back-propagation into
/// unit-variant inference (orthogonal feature gap).
const ENUMERABLE_FIXTURE: &str = "
    struct Counter
      start: Int
      finish: Int
    end

    impl Counter
      fn length(self) -> Int
        self.finish - self.start
      end

      fn get(self, index: Int) -> Option<Int>
        Option.Some(self.start + index)
      end
    end
    ";

fn with_fixture(body: &str) -> String {
    format!("{ENUMERABLE_FIXTURE}\n{body}")
}

#[test]
fn for_over_counter_sums_elements() {
    let source = with_fixture(
        "
        fn main -> Int
          c = Counter{start: 1, finish: 5}
          sum = 0
          for x in c
            sum = sum + x
          end
          sum
        end
        ",
    );
    assert_eq!(evaluate(&dedent(&source)).unwrap(), Value::Int(10));
}

#[test]
fn for_with_zero_length_iterable_skips_body() {
    let source = with_fixture(
        "
        fn main -> Int
          c = Counter{start: 7, finish: 7}
          count = 0
          for _ in c
            count = count + 1
          end
          count
        end
        ",
    );
    assert_eq!(evaluate(&dedent(&source)).unwrap(), Value::Int(0));
}

#[test]
fn early_return_inside_for_exits_function() {
    let source = with_fixture(
        "
        fn main -> Int
          c = Counter{start: 0, finish: 100}
          for x in c
            if x == 7
              return x
            end
          end
          0
        end
        ",
    );
    assert_eq!(evaluate(&dedent(&source)).unwrap(), Value::Int(7));
}

#[test]
fn nested_for_loops_iterate_correctly() {
    let source = with_fixture(
        "
        fn main -> Int
          outer = Counter{start: 0, finish: 3}
          inner = Counter{start: 0, finish: 4}
          total = 0
          for _ in outer
            for _ in inner
              total = total + 1
            end
          end
          total
        end
        ",
    );
    assert_eq!(evaluate(&dedent(&source)).unwrap(), Value::Int(12));
}

#[test]
fn for_value_is_unit() {
    let source = with_fixture(
        "
        fn main
          c = Counter{start: 0, finish: 1}
          for _ in c
          end
        end
        ",
    );
    assert_eq!(evaluate(&dedent(&source)).unwrap(), Value::Unit);
}

#[test]
fn for_with_string_concat_in_body_accumulates_heap_state() {
    // Heap-typed loop-carried slot inside the desugared `for`. Each
    // iteration's `s = s <> "x"` reassignment drops the prior value
    // through the same ownership-aware `LocalWrite` path the `while`
    // tests pin — `for` is a pure desugar so nothing new at runtime.
    let source = with_fixture(
        "
        fn main -> String
          c = Counter{start: 0, finish: 3}
          s = \"\"
          for _ in c
            s = s <> \"x\"
          end
          s
        end
        ",
    );
    assert_eq!(
        evaluate(&dedent(&source)).unwrap(),
        Value::String("xxx".into()),
    );
}

#[test]
fn for_with_if_inside_body_branches_each_iteration() {
    // `if`/`else` inside a `for` body — block-param SSA join still
    // works through the desugared while + match shape and the
    // back-edge into the surrounding header.
    let source = with_fixture(
        "
        fn main -> Int
          c = Counter{start: 0, finish: 6}
          sum = 0
          for x in c
            if x % 2 == 0
              sum = sum + x
            else
              sum = sum + 0
            end
          end
          sum
        end
        ",
    );
    assert_eq!(evaluate(&dedent(&source)).unwrap(), Value::Int(6));
}

#[test]
fn while_with_if_inside_body_branches_each_iteration() {
    // `if`/`else` inside a loop body — block-param SSA join still
    // works even with the back-edge into the surrounding header.
    let source = "
        fn main -> Int
          i = 0
          sum = 0
          while i < 6
            if i % 2 == 0
              sum = sum + i
            else
              sum = sum + 0
            end
            i = i + 1
          end
          sum
        end
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(6));
}
