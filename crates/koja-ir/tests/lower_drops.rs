//! Drop-pipeline regression for the value-semantics reference-counting
//! baseline.
//!
//! Drop glue is enabled: a heap-leaf local (`String` / `Binary` /
//! `Bits`) is reference-counted, so lowering emits a `DropLocal`
//! (`rc--`, freeing at zero) at scope exit. Acquisition sites
//! (`Clone` = `rc++`) keep the count balanced. A binding shared by
//! assignment, returned out of the function, captured in a struct
//! field, or passed as an argument each acquires its own reference, so
//! the per-slot drop is safe rather than a double-free. These tests
//! pin that heap locals carry a `DropLocal` while purely scalar
//! functions carry none, guarding against accidentally dropping the
//! glue (regressing to the old leak baseline) or emitting drops for
//! non-heap slots.

use koja_ir::{IRBasicBlock, IRInstruction};

mod common;

use common::{all_instructions, lower_script_source as lower, script_function};

/// Assert `blocks` emit at least one `DropLocal` (rc drop glue is
/// active for the body's heap-leaf locals).
fn assert_has_drop(blocks: &[IRBasicBlock], name: &str) {
    let has_drop =
        all_instructions(blocks).any(|inst| matches!(inst, IRInstruction::DropLocal { .. }));
    assert!(
        has_drop,
        "`{name}` should emit a DropLocal for its heap-leaf local under the rc baseline",
    );
}

/// Assert `blocks` carry no `DropLocal`, the shape a purely scalar
/// body must keep (no heap slots to reclaim).
fn assert_no_drops(blocks: &[IRBasicBlock], name: &str) {
    for inst in all_instructions(blocks) {
        assert!(
            !matches!(inst, IRInstruction::DropLocal { .. }),
            "`{name}` should emit no DropLocal (no heap locals); got {inst:?}",
        );
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

        taker(\"hi\")
    ";

    let script = lower(source);
    assert_has_drop(&script_function(&script, "taker").blocks, "taker");
}

#[test]
fn returned_heap_local_emits_droplocal() {
    let source = "
        fn shout(prefix: String) -> String
          s = prefix <> \"!\"
          s
        end

        shout(\"hi\")
    ";

    let script = lower(source);
    assert_has_drop(&script_function(&script, "shout").blocks, "shout");
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

        render(\"a\")
    ";

    let script = lower(source);
    assert_has_drop(&script_function(&script, "render").blocks, "render");
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

        classify(0)
    ";

    let script = lower(source);
    assert_has_drop(&script_function(&script, "classify").blocks, "classify");
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

        build()
    ";

    let script = lower(source);
    assert_has_drop(&script_function(&script, "build").blocks, "build");
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

        build()
    ";

    let script = lower(source);
    assert_has_drop(&script_function(&script, "build").blocks, "build");
}

#[test]
fn scalar_program_emits_no_droplocal() {
    let source = "
        fn add(a: Int, b: Int) -> Int
          a + b
        end

        add(1, 2)
    ";

    let script = lower(source);
    assert_no_drops(&script_function(&script, "add").blocks, "add");
    assert_no_drops(&script.blocks, "script body");
}
