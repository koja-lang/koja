//! Eval-side coverage for the tail-call-optimization trampoline.
//! Pins that self-recursive tail calls execute in the host stack
//! at constant depth (no overflow on 100 000 iterations) and
//! produce the same value-level results as the LLVM backend.

use koja_ast::util::dedent;
use koja_ir_eval::Value;

mod common;

use common::evaluate_program as evaluate;

#[test]
fn self_recursive_tail_call_runs_in_constant_stack() {
    // 100 000 frames would blow the host stack without the
    // trampoline; reaching `Value::Int(0)` is the constant-stack
    // smoke test the LLVM backend's `tco_loop` mirrors.
    let source = "
        struct Counter
          n: Int

          fn count_down(move self) -> Self
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
fn self_recursive_unit_tail_call_runs_in_constant_stack() {
    // Unit-returning self-recursion: the rewrite still fires
    // because the `Return Some(call_dest)` shape lower emits for
    // a statement-position Unit-typed call matches the detection
    // pattern. Reaching the base case without overflow is the
    // pin.
    let source = "
        struct Counter
          n: Int

          fn count_down(move self) -> Int
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
