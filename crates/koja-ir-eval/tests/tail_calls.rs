//! Eval-side coverage for the tail-call-optimization trampoline.
//! Pins that self-recursive tail calls execute in the host stack
//! at constant depth (no overflow on 100 000 iterations) and
//! produce the same value-level results as the LLVM backend.

use std::time::{Duration, Instant};

use koja_ast::util::dedent;
use koja_ir_eval::Value;

mod common;

use common::evaluate_program as evaluate;

/// Generous ceiling for the accumulator tests below. A linear build
/// of 100 000 steps finishes well inside it even on a slow CI host,
/// while the quadratic copying path takes minutes.
const ACCUMULATOR_CEILING: Duration = Duration::from_secs(20);

#[test]
fn self_recursive_tail_call_runs_in_constant_stack() {
    // 100 000 frames would blow the host stack without the
    // trampoline. Reaching `Value::Int(0)` is the constant-stack
    // smoke test the LLVM backend's `tco_loop` mirrors.
    let source = "
        struct Counter
          n: Int

          fn count_down(self) -> Self
            if self.n <= 0
              return self
            end

            Counter{n: self.n - 1}.count_down()
          end
        end

        fn main -> Int
          c = Counter{n: 100000}
          result = c.count_down()
          result.n
        end
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(0));
}

#[test]
fn self_recursive_tail_call_as_if_value_runs_in_constant_stack() {
    // No early `return`: the recursive call is the *value* of the
    // `if`, reaching `Return` through a merge-block param. Without the
    // return-forwarder collapse the trampoline never fires and 100 000
    // frames overflow the host stack.
    let source = "
        struct Counter
          n: Int

          fn count_down(self) -> Int
            if self.n <= 0
              0
            else
              Counter{n: self.n - 1}.count_down()
            end
          end
        end

        fn main -> Int
          Counter{n: 100000}.count_down()
        end
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(0));
}

#[test]
fn self_recursive_unit_tail_call_runs_in_constant_stack() {
    // Unit-returning self-recursion: the rewrite still fires
    // because the `Return Some(call_dest)` shape lower emits for
    // a statement-position Unit-typed call matches the detection
    // pattern. Reaching the base case without overflow is the
    // pin.
    let source = "
        struct Counter
          n: Int

          fn count_down(self) -> Int
            if self.n <= 0
              return self.n
            end

            Counter{n: self.n - 1}.count_down()
          end
        end

        fn main -> Int
          Counter{n: 100000}.count_down()
        end
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(0));
}

#[test]
fn try_forwarded_self_call_runs_in_constant_stack() {
    // `try self(...)` in tail position forwards the callee's Result at
    // typecheck instead of unwrapping and rewrapping it, so the
    // recursion loopifies. Without forwarding the `match` desugar sits
    // between the call and the return and 100 000 frames overflow.
    let source = "
        fn count(n: Int, acc: Int) -> Int ! String
          if n == 0
            return acc
          end

          try count(n - 1, acc + 1)
        end

        fn main -> Int
          match count(100000, 0)
            Result.Ok(total) -> total
            Result.Err(_) -> -1
          end
        end
        ";
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(100000));
}

#[test]
fn tail_recursive_list_accumulator_mutates_in_place() {
    // `acc.append(n)` fuses into the consuming twin at the slot exit.
    // The interpreter has to release the param register and the
    // promoted slot before the call, or the twin sees a shared `Rc`
    // and copies the whole list on every step.
    let source = "
        fn build(n: Int, acc: List<Int>) -> List<Int>
          if n == 0
            acc
          else
            build(n - 1, acc.append(n))
          end
        end

        fn main -> Int
          build(100000, []).length()
        end
        ";
    let started = Instant::now();
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(100000));
    let elapsed = started.elapsed();
    assert!(
        elapsed < ACCUMULATOR_CEILING,
        "list accumulator took {elapsed:?}, the twin is copying per step",
    );
}

#[test]
fn tail_recursive_string_accumulator_mutates_in_place() {
    // Same shape over `<>`, which fuses to `Concat { consumes_lhs }`
    // and gates on the same uniqueness check.
    let source = "
        fn build(n: Int, acc: String) -> String
          if n == 0
            acc
          else
            build(n - 1, acc <> \"ab\")
          end
        end

        fn main -> Int
          build(100000, \"\").byte_length()
        end
        ";
    let started = Instant::now();
    assert_eq!(evaluate(&dedent(source)).unwrap(), Value::Int(200000));
    let elapsed = started.elapsed();
    assert!(
        elapsed < ACCUMULATOR_CEILING,
        "string accumulator took {elapsed:?}, the concat is copying per step",
    );
}
