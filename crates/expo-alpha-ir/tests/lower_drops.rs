//! End-to-end smoke coverage for the alpha-foundation `move`/drop
//! pipeline. Each test drives `parse → check → lower` against a
//! source that exercises one of the three drop-pipeline scenarios
//! and pins the resulting IR shape:
//!
//! - **Reassignment-of-Owned**: a `move s: String` slot reassigned
//!   to a literal triggers a `DropLocal` before the new write; the
//!   slot becomes Unowned and produces no fn-exit drop.
//! - **Return-of-Owned**: returning an Owned slot substitutes
//!   `MoveOutLocal` for the standard `LocalRead`, marks the slot
//!   Moved, and skips it at fn-exit drops.
//! - **Move-param flowing through to fn-exit drop**: a `move s: String`
//!   slot the body never consumes is dropped at fn exit (one
//!   `DropLocal` before the terminator).
//!
//! These tests sit one level above `lower_ownership.rs` (which
//! pins per-write stamping) — they validate the full lowerer
//! pipeline including the slot-state tracking, return-path
//! `MoveOutLocal` substitution, and `emit_function_exit_drops`
//! placement.

use expo_alpha_ir::{IRBasicBlock, IRFunction, IRInstruction, IRLocalId, IRTerminator};
use expo_ast::util::dedent;

mod common;

use common::{function, lower_program_source as lower};

fn last_block(function: &IRFunction) -> &IRBasicBlock {
    function
        .blocks
        .last()
        .expect("function should have at least one block")
}

fn move_param_slot(function: &IRFunction, name: &str) -> IRLocalId {
    let param = function.params.first().unwrap_or_else(|| {
        panic!(
            "function `{}` has no params; cannot resolve `{name}`",
            function.symbol
        )
    });
    param.local_id
}

#[test]
fn reassign_owned_slot_emits_droplocal_before_new_write() {
    let source = "
        fn taker(move s: String) -> String
          s = \"literal\"
          s
        end

        fn main
          taker(\"hi\")
        end
    ";

    let program = lower(&dedent(source));
    let taker = function(&program, "taker");
    let s_slot = move_param_slot(taker, "s");

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
        "expected at least two writes to `{s_slot}` (param promotion + reassignment); got {}",
        writes_for_s.len(),
    );

    let drop_index = drops_for_s[0].0;
    let promotion_write = writes_for_s[0].0;
    let reassign_write = writes_for_s[1].0;
    assert!(
        promotion_write < drop_index,
        "DropLocal must come after the promotion write: promotion@{promotion_write}, drop@{drop_index}",
    );
    assert!(
        drop_index < reassign_write,
        "DropLocal must come before the reassignment write: drop@{drop_index}, reassign@{reassign_write}",
    );
}

#[test]
fn return_of_owned_slot_substitutes_moveoutlocal_and_skips_fn_exit_drop() {
    let source = "
        fn shout(move s: String) -> String
          s
        end

        fn main
          shout(\"hi\")
        end
    ";

    let program = lower(&dedent(source));
    let shout = function(&program, "shout");
    let s_slot = move_param_slot(shout, "s");

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
fn unconsumed_move_param_drops_at_fn_exit() {
    let source = "
        fn taker(move s: String) -> Int
          1
        end

        fn main
          taker(\"hi\")
        end
    ";

    let program = lower(&dedent(source));
    let taker = function(&program, "taker");
    let s_slot = move_param_slot(taker, "s");

    let last = last_block(taker);

    let drops: Vec<_> = last
        .instructions
        .iter()
        .enumerate()
        .filter(|(_, i)| matches!(i, IRInstruction::DropLocal { local, .. } if *local == s_slot))
        .collect();
    assert_eq!(
        drops.len(),
        1,
        "expected exactly one fn-exit DropLocal for the unconsumed move param `{s_slot}`; got {}",
        drops.len(),
    );
    let (drop_pos, _) = drops[0];

    assert_eq!(
        drop_pos,
        last.instructions.len() - 1,
        "DropLocal must be the last instruction before the terminator: drop@{drop_pos}, instructions.len()={}",
        last.instructions.len(),
    );

    assert!(
        matches!(last.terminator, IRTerminator::Return { value: Some(_) }),
        "terminator should be Return-with-Int-value; got {:?}",
        last.terminator,
    );
}

#[test]
fn no_owned_slots_means_no_droplocal_or_moveoutlocal() {
    // Smoke regression: existing alpha programs without `move`
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
