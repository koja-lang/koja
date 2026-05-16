//! Lowering coverage for the `<>` concat operator: each of the
//! three heap-payload kinds (`String`, `Binary`, `Bits`) lowers to
//! an [`IRInstruction::Concat`] with the matching [`ConcatKind`],
//! and the concat's destination value carries the matching
//! [`IRType`].
//!
//! Pairs with the typecheck coverage in
//! `expo-typecheck/tests/resolve_ops.rs` (which pins the
//! diagnostic surface for cross-type and non-concat-typed operands)
//! and the eval coverage in `expo-ir-eval/tests/concat.rs`
//! (which pins the runtime byte-for-byte result).

use expo_ast::util::dedent;
use expo_ir::{ConcatKind, IRFunction, IRInstruction, IRType, Ownership};

mod common;

use common::{function, lower_program_source as lower};

fn first_concat(function: &IRFunction) -> &IRInstruction {
    function
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .find(|i| matches!(i, IRInstruction::Concat { .. }))
        .expect("function should contain at least one Concat instruction")
}

#[test]
fn string_concat_lowers_to_concat_string() {
    let source = "
        fn greet(move a: String, move b: String) -> String
          a <> b
        end

        fn main
          1
        end
    ";

    let program = lower(&dedent(source));
    let greet = function(&program, "greet");
    let IRInstruction::Concat { kind, .. } = first_concat(greet) else {
        unreachable!()
    };
    assert_eq!(*kind, ConcatKind::String);
    assert_eq!(greet.return_type, IRType::String);
}

#[test]
fn binary_concat_lowers_to_concat_binary() {
    let source = "
        fn join(move a: Binary, move b: Binary) -> Binary
          a <> b
        end

        fn main
          1
        end
    ";

    let program = lower(&dedent(source));
    let join = function(&program, "join");
    let IRInstruction::Concat { kind, .. } = first_concat(join) else {
        unreachable!()
    };
    assert_eq!(*kind, ConcatKind::Binary);
    assert_eq!(join.return_type, IRType::Binary);
}

#[test]
fn bits_concat_lowers_to_concat_bits() {
    let source = "
        fn join(move a: Bits, move b: Bits) -> Bits
          a <> b
        end

        fn main
          1
        end
    ";

    let program = lower(&dedent(source));
    let join = function(&program, "join");
    let IRInstruction::Concat { kind, .. } = first_concat(join) else {
        unreachable!()
    };
    assert_eq!(*kind, ConcatKind::Bits);
    assert_eq!(join.return_type, IRType::Bits);
}

#[test]
fn concat_result_assigned_to_local_stamps_owned() {
    // `s = "a" <> "b"` should stamp `Ownership::Owned` on the
    // assignment's `LocalWrite` because the RHS is a concat (a
    // heap-allocating producer). This pins the
    // [`expo_ir::lower::ownership::ownership_for_expr`]
    // classifier's `BinOp::Concat` arm — without the Owned stamp,
    // drop emission wouldn't free the freshly-allocated payload.
    let source = "
        fn build(move a: String, move b: String) -> String
          s = a <> b
          s
        end

        fn main
          1
        end
    ";

    let program = lower(&dedent(source));
    let build = function(&program, "build");

    let owned_writes: Vec<_> = build
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .filter_map(|i| match i {
            IRInstruction::LocalWrite {
                local,
                ownership: Ownership::Owned,
                value,
            } => Some((local, value)),
            _ => None,
        })
        .collect();

    // Three Owned LocalWrites: one each for `a` and `b` (move-param
    // promotion), plus one for `s` (the concat result).
    assert_eq!(
        owned_writes.len(),
        3,
        "expected 3 Owned LocalWrites (a, b, s); got {} in {:?}",
        owned_writes.len(),
        build,
    );
}
