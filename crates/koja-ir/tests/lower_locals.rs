//! Coverage for the local-variable slice in `src/lower/`:
//!
//! - Parameter promotion: each declared parameter materializes a
//!   matching `LocalDecl` + `LocalWrite` pair in the entry block, in
//!   declaration order, with the param's `id` flowing into the
//!   `LocalWrite` that mirrors it into its slot.
//! - `Statement::Assignment`: the first write emits `LocalDecl`
//!   followed by `LocalWrite`. Subsequent writes only emit
//!   `LocalWrite` (the slot is already declared).
//! - `ExprKind::Ident` reads of locals lower to `LocalRead` against
//!   the same `IRLocalId` the writes target.
//! - The entry block stays the home of every `LocalDecl` (LLVM relies
//!   on this for single-`alloca` hoisting).
//! - Reassigning a local first declared inside a loop or branch
//!   reuses the slot instead of re-declaring it, and skips the
//!   stale-value drop (the slot may be uninitialized on that path).

use koja_ir::{IRBasicBlock, IRInstruction, IRType};

mod common;

use common::{
    all_instructions, entry_block, local_decls, lower_script_source as lower, script_function,
};

#[test]
fn body_decl_emits_local_decl_then_local_write() {
    let source = "
        x = 42
        x
        ";

    let script = lower(source);
    let entry = entry_block(&script.blocks);
    let body = &entry.instructions;

    let const_pos = body
        .iter()
        .position(|i| matches!(i, IRInstruction::Const { .. }))
        .expect("expected a Const(42) for the rhs");
    let decl_pos = body[const_pos..]
        .iter()
        .position(|i| matches!(i, IRInstruction::LocalDecl { .. }))
        .expect("expected a LocalDecl after the rhs Const")
        + const_pos;
    let write_pos = body[decl_pos..]
        .iter()
        .position(|i| matches!(i, IRInstruction::LocalWrite { .. }))
        .expect("expected a LocalWrite after LocalDecl")
        + decl_pos;
    assert!(
        decl_pos < write_pos,
        "LocalDecl must precede LocalWrite for the same local: got decl@{decl_pos}, write@{write_pos}",
    );

    let IRInstruction::LocalDecl {
        local: decl_local,
        ty,
    } = &body[decl_pos]
    else {
        unreachable!()
    };
    let IRInstruction::LocalWrite {
        local: write_local,
        value,
        ..
    } = &body[write_pos]
    else {
        unreachable!()
    };
    assert_eq!(*ty, IRType::Int64);
    assert_eq!(decl_local, write_local, "decl/write target the same slot");

    let IRInstruction::Const { dest, .. } = &body[const_pos] else {
        unreachable!()
    };
    assert_eq!(*value, *dest, "LocalWrite consumes the rhs Const's dest");

    let read = body
        .iter()
        .find_map(|i| match i {
            IRInstruction::LocalRead { local, dest, ty } => Some((local, dest, ty)),
            _ => None,
        })
        .expect("trailing `x` should lower to a LocalRead");
    assert_eq!(read.0, decl_local, "LocalRead targets the declared slot");
    assert_eq!(*read.2, IRType::Int64);
}

#[test]
fn reassignment_emits_only_local_write() {
    let source = "
        x = 1
        x = 2
        x
        ";

    let script = lower(source);
    let decls = local_decls(&script.blocks);
    assert_eq!(
        decls.len(),
        1,
        "exactly one LocalDecl per slot (reassignment reuses it), got {decls:?}",
    );

    let writes: Vec<_> = all_instructions(&script.blocks)
        .filter_map(|i| match i {
            IRInstruction::LocalWrite { local, .. } => Some(*local),
            _ => None,
        })
        .collect();
    assert_eq!(writes.len(), 2, "two assignments -> two LocalWrites");
    assert_eq!(
        writes[0], writes[1],
        "both writes target the same slot (no shadowing)",
    );
}

#[test]
fn param_promotion_emits_local_decl_and_local_write_in_entry() {
    let source = "
        fn id(n: Int) -> Int
          n
        end

        id(7)
        ";

    let script = lower(source);
    let id_fn = script_function(&script, "id");
    let entry = entry_block(&id_fn.blocks);

    assert_eq!(id_fn.params.len(), 1);
    let param = &id_fn.params[0];

    let decl_pos = entry
        .instructions
        .iter()
        .position(
            |i| matches!(i, IRInstruction::LocalDecl { local, .. } if *local == param.local_id),
        )
        .expect("entry block should declare the param's slot");
    let write_pos = entry
        .instructions
        .iter()
        .position(|i| {
            matches!(
                i,
                IRInstruction::LocalWrite { local, value, .. }
                    if *local == param.local_id && *value == param.id
            )
        })
        .expect("entry block should mirror the param value into its slot");
    assert!(
        decl_pos < write_pos,
        "param promotion must declare before writing: decl@{decl_pos}, write@{write_pos}",
    );

    let read = entry.instructions.iter().find_map(|i| match i {
        IRInstruction::LocalRead { local, ty, .. } if *local == param.local_id => Some(ty),
        _ => None,
    });
    let read_ty = read.expect("body reference should lower to LocalRead on the param's slot");
    assert_eq!(*read_ty, IRType::Int64);
}

/// Every `LocalDecl` in the function must target a distinct slot.
/// Mirrors the seal invariant that panicked before the declared set
/// became monotonic.
fn assert_decl_slots_unique(blocks: &[IRBasicBlock]) {
    let mut seen = Vec::new();
    for inst in local_decls(blocks) {
        let IRInstruction::LocalDecl { local, .. } = inst else {
            unreachable!()
        };
        assert!(
            !seen.contains(local),
            "slot {local} is LocalDecl'd more than once",
        );
        seen.push(*local);
    }
}

#[test]
fn reassign_after_while_reuses_slot_and_skips_stale_drop() {
    // `s` is first declared inside the loop body. The write after
    // the loop must reuse the slot (no second LocalDecl) and must
    // not drop the slot's prior value. On a zero-trip path the slot
    // is uninitialized, and a completed loop already dropped the
    // last iteration's value at the back-edge.
    let source = "
        i = 5
        while i < 3
          s = \"loop\"
          i = i + 1
        end
        s = \"after\"
        s
        ";

    let script = lower(source);
    assert_decl_slots_unique(&script.blocks);

    let string_slot = local_decls(&script.blocks)
        .iter()
        .find_map(|inst| match inst {
            IRInstruction::LocalDecl { local, ty } if *ty == IRType::String => Some(*local),
            _ => None,
        })
        .expect("expected a String LocalDecl for `s`");

    let (write_block, write_pos) = script
        .blocks
        .iter()
        .flat_map(|block| {
            block
                .instructions
                .iter()
                .enumerate()
                .map(move |(pos, inst)| (block, pos, inst))
        })
        .filter_map(|(block, pos, inst)| match inst {
            IRInstruction::LocalWrite { local, .. } if *local == string_slot => Some((block, pos)),
            _ => None,
        })
        .next_back()
        .expect("expected a LocalWrite for the post-loop reassignment");

    let stale_read = write_block.instructions[..write_pos]
        .iter()
        .find_map(|inst| match inst {
            IRInstruction::LocalRead { dest, local, .. } if *local == string_slot => Some(*dest),
            _ => None,
        });
    if let Some(stale) = stale_read {
        let dropped = write_block.instructions[..write_pos]
            .iter()
            .any(|inst| matches!(inst, IRInstruction::DropValue { value, .. } if *value == stale));
        assert!(
            !dropped,
            "reassignment of a not-live slot must not drop the uninitialized prior value",
        );
    }
}

#[test]
fn reassign_after_if_branch_declaration_reuses_slot() {
    let source = "
        c = true
        if c
          n = 1
        end
        n = 2
        n
        ";

    let script = lower(source);
    assert_decl_slots_unique(&script.blocks);
}

#[test]
fn reassign_in_match_arms_after_loop_declaration_reuses_slot() {
    // The shape that surfaced the panic. The loop declares `n`, and
    // both match arms reassign it (resolve still sees the loop's `n`
    // in scope, so all three sites share one slot).
    let source = "
        i = 0
        while i < 6
          n = i * 2
          i = i + n + 1
        end
        r = match i
          1 ->
            n = 100
            n
          2 ->
            n = 200
            n
          _ ->
            0
        end
        r
        ";

    let script = lower(source);
    assert_decl_slots_unique(&script.blocks);
}

#[test]
fn local_decl_lives_in_entry_block_for_if_branch_assignment() {
    // Assignments today are function-scoped (no block-scoping yet),
    // so even when the surface-level decl appears inside an `if`
    // arm the `LocalDecl` must still be hoisted to the entry block.
    // That's the contract LLVM relies on for single-`alloca`
    // hoisting. The corresponding `LocalWrite` stays where the
    // surface assignment was lowered (in the arm).
    let source = "
        if true
          x = 1
        end
        ";

    let script = lower(source);

    let entry = entry_block(&script.blocks);
    let entry_decl = entry
        .instructions
        .iter()
        .find(|i| matches!(i, IRInstruction::LocalDecl { .. }));
    assert!(
        entry_decl.is_some(),
        "LocalDecl must live in the entry block; got entry={:?}",
        entry.instructions,
    );

    // No other block should carry a LocalDecl, only the entry.
    for block in script.blocks.iter().skip(1) {
        for inst in &block.instructions {
            assert!(
                !matches!(inst, IRInstruction::LocalDecl { .. }),
                "non-entry block {} contains a LocalDecl: {inst:?}",
                block.id,
            );
        }
    }
}
