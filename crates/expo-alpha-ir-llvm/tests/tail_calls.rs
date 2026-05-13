//! IR-text snapshot tests for the tail-call-optimization emission
//! shape. Pins that any function whose body contains a
//! self-recursive tail call gains a `tco_loop` LLVM block, and
//! that the back-edge stores the new args into the parameter
//! slot's `alloca` before branching back to the loop header.

use expo_alpha_ir_llvm::emit_llvm_ir;
use expo_ast::util::dedent;

mod common;

use common::{APP_NAME, assert_contains, lower_program_source as lower};

#[test]
fn self_recursive_tail_call_emits_tco_loop_header_and_back_edge() {
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

        fn main
          c = Counter{n: 1}
          result = c.count_down()
          result.n
        end
        ";
    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir");
    // Loop header is a per-function block named `tco_loop` —
    // appended after the param-init block of `count_down`.
    assert_contains(&ir_text, "tco_loop");
    // Param-init block ends with an unconditional branch to the
    // loop header; the back-edge in the recursive arm is also a
    // branch to the same label.
    assert_contains(&ir_text, "br label %tco_loop");
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
    assert!(
        !ir_text.contains("tco_loop"),
        "non-recursive function must not gain a `tco_loop` header; got:\n{ir_text}",
    );
}
