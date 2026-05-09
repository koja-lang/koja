//! IR-text snapshot tests for `while` (and `for`, via the typecheck
//! desugar) lowering through the LLVM emitter. Pins the loop's
//! CFG-shape invariants in LLVM IR text: the three named blocks
//! (`while_header`, `while_body`, `while_exit`) and the `br`
//! instruction that forms the back-edge.

use expo_alpha_ir_llvm::emit_llvm_ir;
use expo_ast::util::dedent;

mod common;

use common::{APP_NAME, assert_contains, assert_main_shape, lower_program_source as lower};

#[test]
fn while_emits_header_body_exit_blocks_and_back_edge() {
    let source = "
        fn main
          i = 0
          while i < 3
            i = i + 1
          end
        end
        ";
    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir");
    assert_main_shape(&ir_text);
    // Each IR block lowers to a labeled LLVM block whose label is
    // the IR block's `label` field. Pin the trio.
    assert_contains(&ir_text, "while_header");
    assert_contains(&ir_text, "while_body");
    assert_contains(&ir_text, "while_exit");
    // Header terminates with a conditional branch (cond=true → body,
    // cond=false → exit).
    assert_contains(&ir_text, "br i1");
}

#[test]
fn for_emits_while_shape_and_calls_iterable_methods() {
    // The for-desugar produces a while-shaped CFG with `Counter.length`
    // / `Counter.get` calls and a match on `Option<Int>` inside the
    // body — same LLVM shape as a hand-written while + match.
    let source = "
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

        fn main
          c = Counter{start: 0, finish: 3}
          for _ in c
          end
        end
        ";
    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir");
    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "while_header");
    assert_contains(&ir_text, "while_body");
    assert_contains(&ir_text, "while_exit");
    assert_contains(&ir_text, "TestApp.Counter.length");
    assert_contains(&ir_text, "TestApp.Counter.get");
}

#[test]
fn while_with_string_concat_emits_alloca_for_loop_carried_slot() {
    // The mutable `s` slot is heap-typed (String). Loop body
    // reassigns it each iteration; the LocalWrite emits a `store`
    // against the slot's `alloca`, and the header's cond-side
    // LocalRead emits a `load`. Pin the alloca presence.
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
    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir");
    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "while_header");
    assert_contains(&ir_text, "while_body");
    assert_contains(&ir_text, "alloca");
    assert_contains(&ir_text, "store");
    assert_contains(&ir_text, "load");
}
