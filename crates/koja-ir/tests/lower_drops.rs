//! End-to-end smoke coverage for the drop pipeline. Each test
//! drives `parse → check → lower` against a source that exercises one
//! of the drop-pipeline scenarios and pins the resulting IR shape:
//!
//! - **Reassignment-of-Owned**: an Owned slot reassigned to a literal
//!   triggers a `DropLocal` before the new write; the slot becomes
//!   Unowned and produces no fn-exit drop.
//! - **Return-of-Owned**: returning an Owned slot substitutes
//!   `MoveOutLocal` for the standard `LocalRead`, marks the slot
//!   Moved, and skips it at fn-exit drops.
//!
//! Owned slots are produced by heap-allocating expressions (string
//! concat). The `move` keyword is inert under value semantics, so
//! parameters are never Owned and never drive these scenarios.
//!
//! These tests sit one level above `lower_ownership.rs` (which
//! pins per-write stamping) — they validate the full lowerer
//! pipeline including the slot-state tracking, return-path
//! `MoveOutLocal` substitution, and `emit_function_exit_drops`
//! placement.

use koja_ast::util::dedent;
use koja_ir::{IRBasicBlock, IRFunction, IRInstruction, IRTerminator, Ownership};

mod common;

use common::{function, lower_program_source as lower};

fn last_block(function: &IRFunction) -> &IRBasicBlock {
    function
        .blocks
        .last()
        .expect("function should have at least one block")
}

/// The local targeted by the function's first Owned `LocalWrite` —
/// the heap-allocating slot the drop scenarios pin on.
fn first_owned_slot(function: &IRFunction) -> koja_ir::IRLocalId {
    function
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .find_map(|i| match i {
            IRInstruction::LocalWrite {
                local,
                ownership: Ownership::Owned,
                ..
            } => Some(*local),
            _ => None,
        })
        .expect("function should have at least one Owned LocalWrite")
}

#[test]
fn reassign_owned_slot_emits_droplocal_before_new_write() {
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
    let taker = function(&program, "taker");
    let s_slot = first_owned_slot(taker);

    let body = &taker
        .blocks
        .first()
        .expect("entry block missing")
        .instructions;

    let drop_pos = body.iter().position(|i| {
        matches!(
            i,
            IRInstruction::DropLocal { local, .. } if *local == s_slot
        )
    });
    assert!(
        drop_pos.is_some(),
        "expected a DropLocal on `{s_slot}` before its reassignment write",
    );

    let drops_for_s: Vec<_> = body
        .iter()
        .enumerate()
        .filter(|(_, i)| matches!(i, IRInstruction::DropLocal { local, .. } if *local == s_slot))
        .collect();
    assert_eq!(
        drops_for_s.len(),
        1,
        "exactly one DropLocal for the reassignment expected; got {} at positions {:?}",
        drops_for_s.len(),
        drops_for_s.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
    );

    let writes_for_s: Vec<_> = body
        .iter()
        .enumerate()
        .filter(|(_, i)| matches!(i, IRInstruction::LocalWrite { local, .. } if *local == s_slot))
        .collect();
    assert!(
        writes_for_s.len() >= 2,
        "expected at least two writes to `{s_slot}` (initial concat + reassignment); got {}",
        writes_for_s.len(),
    );

    let drop_index = drops_for_s[0].0;
    let initial_write = writes_for_s[0].0;
    let reassign_write = writes_for_s[1].0;
    assert!(
        initial_write < drop_index,
        "DropLocal must come after the initial Owned write: initial@{initial_write}, drop@{drop_index}",
    );
    assert!(
        drop_index < reassign_write,
        "DropLocal must come before the reassignment write: drop@{drop_index}, reassign@{reassign_write}",
    );
}

#[test]
fn return_of_owned_slot_substitutes_moveoutlocal_and_skips_fn_exit_drop() {
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
    let shout = function(&program, "shout");
    let s_slot = first_owned_slot(shout);

    let last = last_block(shout);

    let move_outs: Vec<_> = last
        .instructions
        .iter()
        .filter_map(|i| match i {
            IRInstruction::MoveOutLocal { dest, local, .. } if *local == s_slot => Some(*dest),
            _ => None,
        })
        .collect();
    assert_eq!(
        move_outs.len(),
        1,
        "expected exactly one MoveOutLocal for `{s_slot}` on the return path; got {}",
        move_outs.len(),
    );
    let moved_dest = move_outs[0];

    let drops_for_s: Vec<_> = last
        .instructions
        .iter()
        .filter(|i| matches!(i, IRInstruction::DropLocal { local, .. } if *local == s_slot))
        .collect();
    assert!(
        drops_for_s.is_empty(),
        "moved-out slot must NOT receive a fn-exit DropLocal; got {} entries",
        drops_for_s.len(),
    );

    let IRTerminator::Return { value: Some(rv) } = &last.terminator else {
        panic!(
            "expected Return-with-value terminator on the return path, got {:?}",
            last.terminator,
        );
    };
    assert_eq!(
        *rv, moved_dest,
        "Return value must point at the MoveOutLocal's dest, not the original LocalRead",
    );
}

#[test]
fn match_arms_writing_owned_emit_no_droplocal_when_pre_state_unowned() {
    // The escape_debug repro: `result = ""` (Unowned literal) then a
    // `match` whose arms each `result = result <> "..."` (Owned).
    // Before the slot-state snapshot/restore fix, lowering arm 2 saw
    // arm 1's post-state (`Owned`) and synthesized a `DropLocal` at
    // arm 2's body block — but arm 2's body only executes when arm 1
    // did not, so at runtime the slot still holds the literal "" and
    // the `free` SIGABRTs on a rodata pointer.
    //
    // After the fix: every arm starts from the construct-entry
    // snapshot, so no arm sees the slot as `Owned`, and no
    // `DropLocal` is emitted inside any arm body block.
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
    let render = function(&program, "render");

    // No block carries a DropLocal — neither inside any match arm
    // body nor as the function-exit drop, because the trailing
    // `result` move-out marks the slot moved before fn-exit drop
    // emission runs.
    for block in &render.blocks {
        for inst in &block.instructions {
            assert!(
                !matches!(inst, IRInstruction::DropLocal { .. }),
                "function `render` should have no DropLocal after the slot-state \
                 snapshot/restore fix; got {inst:?} in block `{}`",
                block.label,
            );
        }
    }
}

#[test]
fn cond_arms_writing_owned_emit_no_stale_droplocal() {
    // `cond` mirrors `match`'s arm-merge shape: same potential for a
    // cross-arm slot-state leak, same fix applies. Pin that `cond`
    // arms that all promote a slot to `Owned` don't synthesize a
    // stale `DropLocal` against the pre-cond Unowned literal.
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
    let classify = function(&program, "classify");

    for block in &classify.blocks {
        for inst in &block.instructions {
            assert!(
                !matches!(inst, IRInstruction::DropLocal { .. }),
                "function `classify` should have no DropLocal after the slot-state \
                 snapshot/restore fix; got {inst:?} in block `{}`",
                block.label,
            );
        }
    }
}

#[test]
fn match_arms_writing_owned_merge_to_owned_when_every_arm_agrees() {
    // After lowering `result = ""; match c { ... -> result = result <> X }`
    // the merge should adopt `Owned` because every reachable arm
    // wrote an Owned value. The fn body then returns `result`, which
    // moves out via `MoveOutLocal` (the substitution depends on the
    // merged state being `Owned`). Pin that an explicit Return of
    // the slot finds a `MoveOutLocal` in the return block — proves
    // the merge promoted the slot to `Owned`.
    let source = "
        fn render(c: String) -> String
          result = \"\"
          match c
            \"a\" -> result = result <> \"A\"
            _ -> result = result <> c
          end
          result
        end

        fn main
          render(\"a\")
        end
    ";

    let program = lower(&dedent(source));
    let render = function(&program, "render");

    let move_outs: usize = render
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|inst| matches!(inst, IRInstruction::MoveOutLocal { .. }))
        .count();
    assert_eq!(
        move_outs, 1,
        "expected one MoveOutLocal on `result`'s return path (proving the merged \
         post-match state is `Owned`); got {move_outs}",
    );
}

#[test]
fn struct_field_init_from_owned_local_substitutes_moveoutlocal() {
    // The demo_api SIGSEGV: a `<>`-built local moved into a struct
    // field and returned reads back freed memory. Without
    // `move_out_local_value` at the struct-field sink the slot
    // stayed Live & Owned, fn-exit `DropLocal` freed the payload,
    // and the returned struct's field pointed at the freed bytes.
    // Pin that the substitution fires exactly once and no fn-exit
    // drop sneaks in.
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
    let build = function(&program, "build");

    let move_outs: usize = build
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|inst| matches!(inst, IRInstruction::MoveOutLocal { .. }))
        .count();
    assert_eq!(
        move_outs, 1,
        "expected exactly one MoveOutLocal substitution at the struct-field \
         init sink; got {move_outs}",
    );

    let drops: usize = build
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|inst| matches!(inst, IRInstruction::DropLocal { .. }))
        .count();
    assert_eq!(
        drops, 0,
        "moved-out slot must not also receive a fn-exit DropLocal; got {drops}",
    );
}

#[test]
fn arg_from_owned_local_keeps_droplocal_and_no_moveoutlocal() {
    // An Owned local passed as an argument must NOT trigger
    // MoveOutLocal on the caller's slot — under value semantics every
    // argument is passed by borrow, so the caller still owns the
    // payload through the call and the fn-exit `DropLocal` frees it.
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
    let build = function(&program, "build");

    let move_outs: usize = build
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|inst| matches!(inst, IRInstruction::MoveOutLocal { .. }))
        .count();
    assert_eq!(
        move_outs, 0,
        "borrow-mode args must not emit MoveOutLocal on the caller's slot; got {move_outs}",
    );

    let drops: usize = build
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|inst| matches!(inst, IRInstruction::DropLocal { .. }))
        .count();
    assert_eq!(
        drops, 1,
        "Owned local borrowed across a call must still receive its fn-exit DropLocal; got {drops}",
    );
}

#[test]
fn no_owned_slots_means_no_droplocal_or_moveoutlocal() {
    // Smoke regression: existing programs without `move`
    // params or heap-typed assignments should produce zero
    // `DropLocal` and zero `MoveOutLocal` instructions across
    // every block. Guards against accidental drop-on-everything
    // wiring.
    let source = "
        fn add(a: Int, b: Int) -> Int
          a + b
        end

        fn main
          add(1, 2)
        end
    ";

    let program = lower(&dedent(source));
    for fn_name in &["add", "main"] {
        let function = function(&program, fn_name);
        for block in &function.blocks {
            for inst in &block.instructions {
                assert!(
                    !matches!(inst, IRInstruction::DropLocal { .. }),
                    "function `{fn_name}` should have no DropLocal; got {inst:?}",
                );
                assert!(
                    !matches!(inst, IRInstruction::MoveOutLocal { .. }),
                    "function `{fn_name}` should have no MoveOutLocal; got {inst:?}",
                );
            }
        }
    }
}
