//! Lowering coverage for the `Clone` protocol's heap-primitive
//! intrinsics. Pins:
//!
//! - the three `@intrinsic impl` decls land as
//!   [`FunctionKind::Intrinsic`] with the matching
//!   [`IRIntrinsicId`] payload (`String(StringMethod::Clone)`,
//!   `Binary(BinaryMethod::Clone)`, `Bits(BitsMethod::Clone)`);
//! - a user `s.clone()` call lowers to an ordinary
//!   [`IRInstruction::Call`] keyed on the matching mangled symbol
//!   with the receiver as the sole argument.
//!
//! Out of scope here: backend-emitted body shape (eval coverage in
//! `koja-ir-eval/tests/clone.rs`, LLVM coverage in
//! `koja-ir-llvm/tests/clone.rs`). Owned-vs-Unowned stamping
//! on the destination slot is not pinned — today
//! `lower::ownership::ownership_for_expr` only flags constructor-shaped
//! AST nodes (`Concat`, `BinaryLiteral`, closure literals, `Receive`,
//! interpolated strings) as `Owned`; method-call returns are still
//! `Unowned` regardless of fresh-allocating semantics. PR2 (the
//! universal Clone slice) ships the registry-aware classifier that
//! lets `s.clone()` stamp `Owned` end-to-end. Until then, the heap
//! buffer the clone produces is leaked at scope exit — an existing
//! gap shared by every fresh-allocating helper (`String.upcase`,
//! `String.replace`, …), not a regression.

use koja_ast::util::dedent;
use koja_ir::{
    BinaryMethod, BitsMethod, FunctionKind, IRFunction, IRInstruction, IRIntrinsicId, IRProgram,
    StringMethod,
};

mod common;

use common::lower_program_source as lower;

fn intrinsic_function<'a>(program: &'a IRProgram, mangled: &str) -> &'a IRFunction {
    program
        .function(mangled)
        .unwrap_or_else(|| panic!("missing intrinsic `{mangled}` in IRProgram"))
}

#[test]
fn string_clone_lowers_as_intrinsic() {
    let program = lower(&dedent(
        "
        fn main -> String
          \"hi\".clone()
        end
        ",
    ));
    let function = intrinsic_function(&program, "Global.String.clone");
    let FunctionKind::Intrinsic(id) = &function.kind else {
        panic!(
            "String.clone should lower as Intrinsic; got {:?}",
            function.kind,
        );
    };
    assert_eq!(*id, IRIntrinsicId::String(StringMethod::Clone));
    assert!(
        function.blocks.is_empty(),
        "intrinsic decl should lower to zero blocks; got {}",
        function.blocks.len(),
    );
}

#[test]
fn binary_clone_lowers_as_intrinsic() {
    let program = lower(&dedent(
        "
        fn from_bytes() -> Binary
          \"hi\".to_binary()
        end

        fn main -> Binary
          from_bytes().clone()
        end
        ",
    ));
    let function = intrinsic_function(&program, "Global.Binary.clone");
    let FunctionKind::Intrinsic(id) = &function.kind else {
        panic!(
            "Binary.clone should lower as Intrinsic; got {:?}",
            function.kind,
        );
    };
    assert_eq!(*id, IRIntrinsicId::Binary(BinaryMethod::Clone));
}

#[test]
fn bits_clone_lowers_as_intrinsic() {
    let program = lower(&dedent(
        "
        fn from_bytes() -> Bits
          \"hi\".to_binary().to_bits()
        end

        fn main -> Bits
          from_bytes().clone()
        end
        ",
    ));
    let function = intrinsic_function(&program, "Global.Bits.clone");
    let FunctionKind::Intrinsic(id) = &function.kind else {
        panic!(
            "Bits.clone should lower as Intrinsic; got {:?}",
            function.kind,
        );
    };
    assert_eq!(*id, IRIntrinsicId::Bits(BitsMethod::Clone));
}

#[test]
fn string_clone_call_site_lowers_to_call_of_intrinsic() {
    let program = lower(&dedent(
        "
        fn main -> String
          \"hi\".clone()
        end
        ",
    ));
    let main = program
        .function("TestApp.main")
        .expect("main missing in IRProgram");

    let call = main
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .find_map(|inst| match inst {
            IRInstruction::Call { callee, args, dest } => Some((callee, args, dest)),
            _ => None,
        })
        .expect("expected a Call instruction in main");
    assert_eq!(
        call.0.mangled(),
        "Global.String.clone",
        "call site should target the String.clone intrinsic",
    );
    assert_eq!(call.1.len(), 1, "clone takes a single `self` argument");
}

#[test]
fn binding_to_clone_result_writes_a_local_slot() {
    // The destination ownership stamp is intentionally left flexible
    // here — see the module comment for the deferred-Owned story.
    // What we *do* pin is that the clone result lands in a real
    // `LocalWrite`, so the slot is reachable for the upcoming
    // ownership-lattice extension to flip without touching the
    // lowering shape.
    let program = lower(&dedent(
        "
        fn main -> Int
          copy = \"hi\".clone()
          1
        end
        ",
    ));
    let main = program
        .function("TestApp.main")
        .expect("main missing in IRProgram");

    let local_writes: Vec<_> = main
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .filter(|inst| matches!(inst, IRInstruction::LocalWrite { .. }))
        .collect();
    assert!(
        !local_writes.is_empty(),
        "expected at least one LocalWrite for the `copy = ...` binding",
    );
}
