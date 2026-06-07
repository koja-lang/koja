//! Drop-pipeline regression for the value-semantics reference-counting
//! baseline.
//!
//! Drop glue is enabled: a heap-leaf local (`String` / `Binary` /
//! `Bits`) is reference-counted, so lowering emits a `DropLocal`
//! (`rc--`, freeing at zero) at scope exit. Acquisition sites
//! (`Clone` = `rc++`) keep the count balanced — a binding shared by
//! assignment, returned out of the function, captured in a struct
//! field, or passed as an argument each acquires its own reference, so
//! the per-slot drop is safe rather than a double-free. These tests
//! pin that heap locals carry a `DropLocal` while purely scalar
//! functions carry none, guarding against accidentally dropping the
//! glue (regressing to the old leak baseline) or emitting drops for
//! non-heap slots.

use koja_ast::util::dedent;
use koja_ir::{IRFunction, IRInstruction};

mod common;

use common::{function, lower_program_source as lower};

/// Assert `function` emits at least one `DropLocal` (rc drop glue is
/// active for its heap-leaf locals).
fn assert_has_drop(function: &IRFunction, name: &str) {
    let has_drop = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|inst| matches!(inst, IRInstruction::DropLocal { .. }));
    assert!(
        has_drop,
        "function `{name}` should emit a DropLocal for its heap-leaf local under the rc baseline",
    );
}

/// Assert `function` carries no `DropLocal` in any block — the shape a
/// purely scalar function must keep (no heap slots to reclaim).
fn assert_no_drops(function: &IRFunction, name: &str) {
    for block in &function.blocks {
        for inst in &block.instructions {
            assert!(
                !matches!(inst, IRInstruction::DropLocal { .. }),
                "function `{name}` should emit no DropLocal (no heap locals); \
                 got {inst:?} in block `{}`",
                block.label,
            );
        }
    }
}

#[test]
fn reassigned_heap_local_emits_droplocal() {
    let source = "
        fn taker(prefix: String) -> String
          s = prefix <> \"!\"
          s = \"literal\"
          s
        end

        fn main
          taker(\"hi\")
        end
    ";

    let program = lower(&dedent(source));
    assert_has_drop(function(&program, "taker"), "taker");
}

#[test]
fn returned_heap_local_emits_droplocal() {
    let source = "
        fn shout(prefix: String) -> String
          s = prefix <> \"!\"
          s
        end

        fn main
          shout(\"hi\")
        end
    ";

    let program = lower(&dedent(source));
    assert_has_drop(function(&program, "shout"), "shout");
}

#[test]
fn match_arms_writing_heap_emit_droplocal() {
    let source = "
        fn render(c: String) -> String
          result = \"\"
          match c
            \"a\" -> result = result <> \"A\"
            \"b\" -> result = result <> \"B\"
            _ -> result = result <> c
          end
          result
        end

        fn main
          render(\"a\")
        end
    ";

    let program = lower(&dedent(source));
    assert_has_drop(function(&program, "render"), "render");
}

#[test]
fn cond_arms_writing_heap_emit_droplocal() {
    let source = "
        fn classify(n: Int) -> String
          result = \"\"
          cond
            n < 0 -> result = result <> \"neg\"
            n > 0 -> result = result <> \"pos\"
            else -> result = result <> \"zero\"
          end
          result
        end

        fn main
          classify(0)
        end
    ";

    let program = lower(&dedent(source));
    assert_has_drop(function(&program, "classify"), "classify");
}

#[test]
fn struct_field_init_from_heap_local_emits_droplocal() {
    let source = "
        struct Box
          body: String
        end

        fn build -> Box
          text = \"hi\" <> \"!\"
          Box{body: text}
        end

        fn main
          build()
        end
    ";

    let program = lower(&dedent(source));
    assert_has_drop(function(&program, "build"), "build");
}

#[test]
fn heap_local_passed_as_arg_emits_droplocal() {
    let source = "
        fn examine(s: String) -> Int
          s.length()
        end

        fn build -> Int
          text = \"hi\" <> \"!\"
          examine(text)
        end

        fn main
          build()
        end
    ";

    let program = lower(&dedent(source));
    assert_has_drop(function(&program, "build"), "build");
}

#[test]
fn scalar_program_emits_no_droplocal() {
    let source = "
        fn add(a: Int, b: Int) -> Int
          a + b
        end

        fn main
          add(1, 2)
        end
    ";

    let program = lower(&dedent(source));
    assert_no_drops(function(&program, "add"), "add");
    assert_no_drops(function(&program, "main"), "main");
}
