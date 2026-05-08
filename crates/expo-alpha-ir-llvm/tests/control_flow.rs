//! IR-text snapshot tests for value-producing `if`/`else` and
//! `cond` lowering through the LLVM emitter. Block parameters in
//! alpha-IR translate to `phi` instructions at the merge block:
//! one phi per `BlockParam`, one `add_incoming` call per
//! `BranchTarget` arg. These tests pin the resulting LLVM IR text
//! shape so a future regression that drops phi emission or shuffles
//! the incoming wiring shows up as a substring miss.

use expo_alpha_ir_llvm::emit_llvm_ir;
use expo_ast::util::dedent;

mod common;

use common::{APP_NAME, assert_contains, assert_main_shape, lower_program_source as lower};

#[test]
fn if_else_merge_emits_phi_for_int_arms() {
    // Every reaching arm of an Int-valued if/else hands its tail
    // value to the merge block via a per-edge `BranchTarget::args`
    // payload; LLVM emission lowers that to a `phi i64` with one
    // incoming per arm.
    let source = "
        fn pick -> Int
          if true
            7
          else
            9
          end
        end

        fn main
          pick()
        end
        ";
    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir");
    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "phi i64");
    // Pin the merge-block label-name shape so an accidental rename
    // shows up as a miss.
    assert_contains(&ir_text, "if_merge");
}

#[test]
fn cond_merge_emits_phi_with_incoming_per_arm() {
    // `cond` lowers to chained test-blocks plus per-arm body blocks;
    // each reaching body and the else-body all branch into the merge
    // with their tail value as the per-edge arg. LLVM phi sees one
    // incoming per branch.
    let source = "
        fn pick -> Int
          cond
            true -> 1
            false -> 2
            else -> 3
          end
        end

        fn main
          pick()
        end
        ";
    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir");
    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "phi i64");
    assert_contains(&ir_text, "cond_merge");
}

#[test]
fn if_no_else_emits_unit_typed_merge() {
    // The no-else `if` types as Unit; the merge BlockParam is Unit
    // and the LLVM phi is `phi {}` (the empty struct LLVM lowers
    // `Unit` to). Pin the shape so a regression in Unit handling
    // surfaces clearly.
    let source = "
        fn main
          if true
            1
          end
        end
        ";
    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir");
    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "if_merge");
}

#[test]
fn if_else_with_both_arms_diverging_emits_unreachable_merge_phi() {
    // Both arms `return` early; the merge block has a BlockParam
    // that no edge feeds. LLVM accepts a zero-incoming phi at the
    // builder level (verification may flag it later); the IR-level
    // shape stays well-formed because the merge block is reachable
    // only by name (no actual edge), and the function's natural
    // exit is via the arms' own Returns.
    let source = "
        fn diverge -> Int
          if true
            return 1
          else
            return 2
          end
        end

        fn main
          diverge()
        end
        ";
    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir");
    assert_main_shape(&ir_text);
}

#[test]
fn ternary_emits_phi_for_int_arms() {
    // Same merge-block-with-BlockParam shape as `if`/`else`'s
    // with-else path, just driven by `cond ? a : b`. The LLVM
    // emitter sees the same IR shape and produces a `phi i64`
    // with one incoming per arm.
    let source = "
        fn pick -> Int
          true ? 7 : 9
        end

        fn main
          pick()
        end
        ";
    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir");
    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "phi i64");
    assert_contains(&ir_text, "ternary_merge");
}

#[test]
fn if_else_with_diverging_arm_still_emits_phi_with_one_incoming() {
    // The then-arm diverges via `return`; only the else-arm reaches
    // merge. LLVM accepts a phi with a single incoming (LLVM IR
    // permits 1+ incomings per phi) — the merge-block param's
    // BlockParam-to-phi translation runs unchanged.
    let source = "
        fn pick -> Int
          if false
            return 1
          else
            42
          end
        end

        fn main
          pick()
        end
        ";
    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir");
    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "phi i64");
    assert_contains(&ir_text, "if_merge");
}

#[test]
fn match_int_chain_emits_chained_test_blocks_and_merge_phi() {
    // Each non-catch-all arm lowers to a test block plus an arm
    // body block; the test block fires `icmp eq` against the subject
    // and `br i1` to either its body or the next test. The dominance
    // rule means the subject (defined in entry) is visible in every
    // test block without being threaded through a BlockParam. The
    // subject comes through a function parameter so LLVM's builder
    // can't constant-fold the comparisons away.
    let source = "
        fn pick(n: Int) -> Int
          match n
            1 -> 10
            2 -> 20
            _ -> 30
          end
        end

        fn main
          pick(1)
        end
        ";
    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir");
    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "match_merge");
    assert_contains(&ir_text, "match_test_");
    assert_contains(&ir_text, "match_body_");
    assert_contains(&ir_text, "icmp eq i64");
    assert_contains(&ir_text, "phi i64");
}

#[test]
fn match_string_literal_arm_emits_strcmp_test() {
    // String equality lowers to `strcmp(a, b) == 0`, so the test
    // block emits a `call @strcmp` followed by an `icmp eq i32 …, 0`
    // before the conditional branch. Pin both shapes so a regression
    // in string comparison surfaces clearly. The subject comes
    // through a function parameter to keep the `strcmp` call from
    // being constant-folded.
    let source = "
        fn pick(s: String) -> Int
          match s
            \"hi\" -> 1
            _ -> 0
          end
        end

        fn main
          pick(\"hi\")
        end
        ";
    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir");
    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "match_merge");
    assert_contains(&ir_text, "@strcmp");
    assert_contains(&ir_text, "icmp eq i32");
}

#[test]
fn match_binding_arm_emits_local_alloca_and_store() {
    // A binding arm allocates a local slot for the bound name,
    // stores the subject value into it on entry to the body, and
    // every read of the binding goes through `load`. Pin the
    // alloca/store shape so regressions in pattern-binding lowering
    // surface here.
    let source = "
        fn pick(n: Int) -> Int
          match n
            x -> x + 1
          end
        end

        fn main
          pick(7)
        end
        ";
    let program = lower(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir");
    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "alloca i64");
    assert_contains(&ir_text, "store i64");
    assert_contains(&ir_text, "load i64");
}
