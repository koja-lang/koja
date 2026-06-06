//! Lowering coverage for the `<>` concat operator: each of the
//! three heap-payload kinds (`String`, `Binary`, `Bits`) lowers to
//! an [`IRInstruction::Concat`] with the matching [`ConcatKind`],
//! and the concat's destination value carries the matching
//! [`IRType`].
//!
//! Pairs with the typecheck coverage in
//! `koja-typecheck/tests/resolve_ops.rs` (which pins the
//! diagnostic surface for cross-type and non-concat-typed operands)
//! and the eval coverage in `koja-ir-eval/tests/concat.rs`
//! (which pins the runtime byte-for-byte result).

use koja_ast::util::dedent;
use koja_ir::{ConcatKind, IRFunction, IRInstruction, IRType};

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
        fn greet(a: String, b: String) -> String
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
        fn join(a: Binary, b: Binary) -> Binary
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
        fn join(a: Bits, b: Bits) -> Bits
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
