//! Coverage for the local-variable slice in `src/lower/`:
//!
//! - Parameter promotion: each declared parameter materializes a
//!   matching `LocalDecl` + `LocalWrite` pair in the entry block, in
//!   declaration order, with the param's `id` flowing into the
//!   `LocalWrite` that mirrors it into its slot.
//! - `Statement::Assignment`: the first write emits `LocalDecl`
//!   followed by `LocalWrite`; subsequent writes only emit
//!   `LocalWrite` (the slot is already declared).
//! - `ExprKind::Ident` reads of locals lower to `LocalRead` against
//!   the same `IRLocalId` the writes target.
//! - The entry block stays the home of every `LocalDecl` (LLVM relies
//!   on this for single-`alloca` hoisting).

use koja_ast::util::dedent;
use koja_ir::{IRFunction, IRInstruction, IRType};

mod common;

use common::{function, lower_program_source as lower};

fn entry_block(function: &IRFunction) -> &koja_ir::IRBasicBlock {
    function
        .blocks
        .first()
        .expect("function should have at least one block")
}

fn local_decls(function: &IRFunction) -> Vec<&IRInstruction> {
    function
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .filter(|i| matches!(i, IRInstruction::LocalDecl { .. }))
        .collect()
}

#[test]
fn body_decl_emits_local_decl_then_local_write() {
    let source = "
        fn main
          x = 42
          x
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    let entry = entry_block(main);
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
        fn main
          x = 1
          x = 2
          x
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    let decls = local_decls(main);
    assert_eq!(
        decls.len(),
        1,
        "exactly one LocalDecl per slot — reassignment reuses it; got {decls:?}",
    );

    let writes: Vec<_> = main
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .filter_map(|i| match i {
            IRInstruction::LocalWrite { local, .. } => Some(*local),
            _ => None,
        })
        .collect();
    assert_eq!(writes.len(), 2, "two assignments → two LocalWrites");
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

        fn main
          id(7)
        end
        ";

    let program = lower(&dedent(source));
    let id_fn = function(&program, "id");
    let entry = entry_block(id_fn);

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

#[test]
fn local_decl_lives_in_entry_block_for_if_branch_assignment() {
    // Assignments today are function-scoped (no block-scoping yet),
    // so even when the surface-level decl appears inside an `if`
    // arm the `LocalDecl` must still be hoisted to the entry block
    // — that's the contract LLVM relies on for single-`alloca`
    // hoisting. The corresponding `LocalWrite` stays where the
    // surface assignment was lowered (in the arm).
    let source = "
        fn main
          if true
            x = 1
          end
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");

    let entry = entry_block(main);
    let entry_decl = entry
        .instructions
        .iter()
        .find(|i| matches!(i, IRInstruction::LocalDecl { .. }));
    assert!(
        entry_decl.is_some(),
        "LocalDecl must live in the entry block; got entry={:?}",
        entry.instructions,
    );

    // No other block should carry a LocalDecl — only the entry.
    for block in main.blocks.iter().skip(1) {
        for inst in &block.instructions {
            assert!(
                !matches!(inst, IRInstruction::LocalDecl { .. }),
                "non-entry block {} contains a LocalDecl: {inst:?}",
                block.id,
            );
        }
    }
}
