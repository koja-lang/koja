//! IR-text snapshot tests for the tail-call-optimization emission
//! shape. Pins that any function whose body contains a
//! self-recursive tail call gains a `tco_loop` LLVM block, and
//! that the back-edge stores the new args into the parameter
//! slot's `alloca` before branching back to the loop header.

use koja_ast::util::dedent;
use koja_ir_llvm::emit_llvm_ir;

mod common;

use common::{APP_NAME, assert_contains, extract_function_body, lower_program_source as lower};

#[test]
fn self_recursive_tail_call_emits_tco_loop_header_and_back_edge() {
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

        fn main
          c = Counter{n: 1}
          result = c.count_down()
          result.n
        end
        ";
    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir");
    // Loop header is a per-function block named `tco_loop`,
    // appended after the param-init block of `count_down`.
    assert_contains(&ir_text, "tco_loop");
    // Param-init block ends with an unconditional branch to the
    // loop header. The back-edge in the recursive arm is also a
    // branch to the same label.
    assert_contains(&ir_text, "br label %tco_loop");
}

#[test]
fn self_recursive_tail_call_as_if_value_is_optimized() {
    // The recursive call is the *value* of the `if` (no early
    // `return`), so it reaches the function's `Return` through a merge
    // block param. The return-forwarder collapse must still expose it
    // as a tail call, otherwise long-running `receive` loops grow the
    // stack one frame per iteration.
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

        fn main
          Counter{n: 3}.count_down()
        end
        ";
    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir");
    let body = extract_function_body(&ir_text, "TestApp.Counter.count_down");
    assert!(
        body.contains("tco_loop"),
        "if-wrapped self-call must gain a tco_loop header; got:\n{body}",
    );
    assert!(
        !body.contains("call i64 @TestApp.Counter.count_down"),
        "no self-`call` may survive after TCO; got:\n{body}",
    );
}

#[test]
fn self_recursive_tail_call_as_match_value_is_optimized() {
    let source = "
        struct Counter
          n: Int

          fn count_down(self) -> Int
            match self.n
              0 -> 0
              _ -> Counter{n: self.n - 1}.count_down()
            end
          end
        end

        fn main
          Counter{n: 3}.count_down()
        end
        ";
    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir");
    let body = extract_function_body(&ir_text, "TestApp.Counter.count_down");
    assert!(
        body.contains("tco_loop"),
        "match-wrapped self-call must gain a tco_loop header; got:\n{body}",
    );
    assert!(
        !body.contains("call i64 @TestApp.Counter.count_down"),
        "no self-`call` may survive after TCO; got:\n{body}",
    );
}

#[test]
fn tail_call_back_edge_zeroes_body_slots() {
    // The back-edge must reset every non-parameter slot to zero so an
    // iteration that doesn't revisit a slot's declaring block (e.g. a
    // different `receive`/`match` arm) exit-drops zero instead of the
    // previous iteration's stale, already-released value.
    let source = r#"
        struct Counter
          n: Int

          fn count_down(self) -> Int
            if self.n <= 0
              0
            else
              s = "x" <> "y"
              Counter{n: self.n - s.length()}.count_down()
            end
          end
        end

        fn main
          Counter{n: 4}.count_down()
        end
        "#;
    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir");
    let body = extract_function_body(&ir_text, "TestApp.Counter.count_down");
    assert!(body.contains("tco_loop"), "expected TCO loop; got:\n{body}");
    // `s` is a heap (`ptr`) slot: one null store for the decl-site
    // zero-init plus one for the back-edge reset.
    let null_stores = body.matches("store ptr null").count();
    assert!(
        null_stores >= 2,
        "expected decl zero-init + back-edge reset null stores for `s`, \
         found {null_stores}; got:\n{body}",
    );
}

#[test]
fn non_recursive_function_emits_no_tco_loop() {
    let source = "
        fn add_one(n: Int) -> Int
          n + 1
        end

        fn main
          add_one(41)
        end
        ";
    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir");
    // Scope to `add_one` itself, since auto-imported stdlib may carry its
    // own (now-optimized) recursive loops elsewhere in the module.
    let body = extract_function_body(&ir_text, "TestApp.add_one");
    assert!(
        !body.contains("tco_loop"),
        "non-recursive function must not gain a `tco_loop` header; got:\n{body}",
    );
}
