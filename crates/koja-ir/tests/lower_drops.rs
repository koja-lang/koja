//! Drop-pipeline regression for the value-semantics leak baseline.
//!
//! Drop insertion is deferred to the future drop-glue pass. Until it
//! lands, lowering emits **zero** `DropLocal`s: a binding shared by
//! assignment (`b = a`) aliases the same heap payload, so freeing
//! per-slot at scope exit would double-free. These tests pin that no
//! shape — heap reassignment, return of a heap local, `match` / `cond`
//! arms, struct-field init, call args, scalar programs — emits a
//! `DropLocal`, guarding against accidentally re-enabling drops before
//! deep-copy-on-acquisition makes them safe.

use koja_ast::util::dedent;
use koja_ir::{IRFunction, IRInstruction};

mod common;

use common::{function, lower_program_source as lower};

/// Assert `function` carries no `DropLocal` in any block.
fn assert_no_drops(function: &IRFunction, name: &str) {
    for block in &function.blocks {
        for inst in &block.instructions {
            assert!(
                !matches!(inst, IRInstruction::DropLocal { .. }),
                "function `{name}` should emit no DropLocal under the leak baseline; \
                 got {inst:?} in block `{}`",
                block.label,
            );
        }
    }
}

#[test]
fn reassigned_heap_local_emits_no_droplocal() {
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
    assert_no_drops(function(&program, "taker"), "taker");
}

#[test]
fn returned_heap_local_emits_no_droplocal() {
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
    assert_no_drops(function(&program, "shout"), "shout");
}

#[test]
fn match_arms_writing_heap_emit_no_droplocal() {
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
    assert_no_drops(function(&program, "render"), "render");
}

#[test]
fn cond_arms_writing_heap_emit_no_droplocal() {
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
    assert_no_drops(function(&program, "classify"), "classify");
}

#[test]
fn struct_field_init_from_heap_local_emits_no_droplocal() {
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
    assert_no_drops(function(&program, "build"), "build");
}

#[test]
fn heap_local_passed_as_arg_emits_no_droplocal() {
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
    assert_no_drops(function(&program, "build"), "build");
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
