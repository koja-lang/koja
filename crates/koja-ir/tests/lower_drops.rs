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
fn owned_string_comparison_operand_emits_dropvalue() {
    // `s <> "!"` produces an owned temp consumed by `==`, which
    // yields a Bool. The temp must be released at the comparison or
    // it leaks (regression: `String.split`'s slice-and-compare loop
    // leaked one block per scanned character).
    let source = "
        fn check(s: String) -> Bool
          s <> \"!\" == \"hi!\"
        end

        check(\"hi\")
    ";

    let script = lower(source);
    let blocks = &script_function(&script, "check").blocks;
    let has_value_drop =
        all_instructions(blocks).any(|inst| matches!(inst, IRInstruction::DropValue { .. }));
    assert!(
        has_value_drop,
        "`check` should release the owned concat temp consumed by `==`",
    );
}

#[test]
fn arm_scoped_heap_binding_dropped_at_join() {
    // `line` exists only inside the match arm, so it doesn't survive
    // the post-match slot merge and function-exit drops never see it.
    // The arm's tail must release it before branching to the merge
    // (regression: every HTTP.Parser `read_line` leaked its line).
    let source = "
        fn make(n: Int, raw: String) -> Option<String>
          match n > 0
            true ->
              line = raw <> \"!\"
              Option.Some(line)
            false ->
              Option.None
          end
        end

        make(1, \"hello\")
    ";

    let script = lower(source);
    let function = script_function(&script, "make");
    let arm_drops_line = function
        .blocks
        .iter()
        .filter(|block| block.label.starts_with("match_body"))
        .flat_map(|block| &block.instructions)
        .any(|inst| matches!(inst, IRInstruction::DropLocal { .. }));
    assert!(
        arm_drops_line,
        "`make` should release the arm-scoped `line` slot at the arm tail",
    );
}

#[test]
fn early_return_from_match_arm_releases_owned_subject() {
    // `self.get(i).unwrap()` produces an owned temp subject. The
    // `return` arm exits before the arm tail's subject release runs,
    // so the exit path must carry a DropValue for it (regression:
    // `String.trim_start` leaked one char per scanned byte).
    let source = "
        fn first_word(s: String) -> String
          match s <> \"!\"
            \" \" -> return \"space\"
            _ -> ()
          end
          \"other\"
        end

        first_word(\"hi\")
    ";

    let script = lower(source);
    let function = script_function(&script, "first_word");
    let return_arm_drops_subject = function
        .blocks
        .iter()
        .filter(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, IRInstruction::DropValue { .. }))
        })
        .any(|block| matches!(block.terminator, koja_ir::IRTerminator::Return { .. }));
    assert!(
        return_arm_drops_subject,
        "the `return` arm should release the owned match subject on its exit path",
    );
}

#[test]
fn pattern_bind_slot_is_never_dropped() {
    // `Option.Some(v)` writes the subject's payload storage into `v`
    // without a Clone, so the slot borrows and no drop site may free
    // it (regression: arm-join drops double-freed every bind payload).
    let source = "
        fn take(o: Option<String>) -> Int
          match o
            Option.Some(v) -> v.length()
            Option.None -> 0
          end
        end

        take(Option.Some(\"hello\"))
    ";

    let script = lower(source);
    let function = script_function(&script, "take");
    // The `o` param slot's release elaborates into a `$drop$` call
    // (composite enum), so any remaining `DropLocal` could only be
    // the heap-leaf `v` slot, which must never be freed.
    let drops_bind_slot = all_instructions(&function.blocks)
        .any(|inst| matches!(inst, IRInstruction::DropLocal { .. }));
    assert!(
        !drops_bind_slot,
        "`take` must never drop the borrowed bind `v`",
    );
}

#[test]
fn union_widened_owned_arg_is_released_after_call() {
    // Widening moves the owned `String` temp into the union without
    // an acquire, so the ownership stamp must transfer. The call's
    // temp-release site then frees the union and with it the payload
    // (regression: every `socket.write(serialized)` against a
    // `Binary | String` param leaked the widened string).
    let source = "
        fn sink(data: Binary | String) -> Int
          match data
            b: Binary -> b.byte_size()
            s: String -> s.length()
          end
        end

        fn go(prefix: String) -> Int
          sink(prefix <> \"!\")
        end

        go(\"hi\")
    ";

    let script = lower(source);
    let function = script_function(&script, "go");
    // Elaborate has already rewritten the composite `DropValue` into
    // the synthesized `$drop$` call by the time the script seals.
    let drops_union_temp = all_instructions(&function.blocks).any(|inst| {
        matches!(
            inst,
            IRInstruction::Call { callee, .. }
                if callee.mangled().starts_with("Union_") && callee.mangled().ends_with(".$drop$")
        )
    });
    assert!(
        drops_union_temp,
        "`go` should release the owned union temp after the `sink` call",
    );
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
