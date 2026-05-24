//! Coverage for the foundation `move`/drop pipeline at the
//! lowering layer:
//!
//! - Every existing program shape stamps `Ownership::Unowned` on its
//!   `LocalWrite` instructions (the pipeline pre-filters non-heap types at
//!   the classifier — no expression today produces a heap-typed
//!   value, so every site is Unowned).
//! - `move c: String` parameter promotion stamps `Ownership::Owned`
//!   on the slot and `move c: Int` (a stack type) stamps `Unowned`.
//!   This pins the heap-type-aware classifier and matches the pipeline's
//!   pre-filter approach (non-heap types never carry the Owned
//!   stamp; v1 filters at drop-emission instead).
//!
//! These tests sit at the IR-shape boundary — they don't exercise
//! the LLVM backend or eval. The smoke tests in
//! `tests/lower_drops.rs` cover end-to-end drop-pipeline shape.

use koja_ast::util::dedent;
use koja_ir::{IRFunction, IRInstruction, IRType, Ownership};

mod common;

use common::{function, lower_program_source as lower};

fn local_writes(function: &IRFunction) -> Vec<&IRInstruction> {
    function
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .filter(|i| matches!(i, IRInstruction::LocalWrite { .. }))
        .collect()
}

fn assert_all_unowned(function: &IRFunction, label: &str) {
    let writes = local_writes(function);
    assert!(
        !writes.is_empty(),
        "{label}: function should have at least one LocalWrite to validate",
    );
    for write in &writes {
        let IRInstruction::LocalWrite {
            local, ownership, ..
        } = write
        else {
            unreachable!()
        };
        assert_eq!(
            *ownership,
            Ownership::Unowned,
            "{label}: LocalWrite for `{local}` must stamp Unowned (slot type is non-heap), got {ownership:?}",
        );
    }
}

#[test]
fn body_assignment_stamps_local_write_unowned() {
    let source = "
        fn main
          x = 42
          x
        end
    ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    assert_all_unowned(main, "body assignment");
}

#[test]
fn default_borrow_param_promotion_stamps_unowned() {
    let source = "
        fn id(x: Int) -> Int
          x
        end

        fn main
          id(1)
        end
    ";

    let program = lower(&dedent(source));
    let id = function(&program, "id");
    assert_all_unowned(id, "default-borrow Int param");
}

#[test]
fn move_param_with_stack_type_stamps_unowned() {
    // `move c: Int` is a no-op semantically: `Int` is a copy type,
    // there's no heap to free. The pipeline pre-filters at the
    // classifier and stamps Unowned. This pins that behavior so
    // accidentally rewiring the classifier to "stamp Owned
    // uniformly" gets caught immediately.
    let source = "
        fn taker(move c: Int) -> Int
          c
        end

        fn main
          taker(1)
        end
    ";

    let program = lower(&dedent(source));
    let taker = function(&program, "taker");
    assert_all_unowned(taker, "move Int param");
}

#[test]
fn move_param_with_string_type_stamps_owned_on_promotion() {
    // `move s: String` is the only path today that produces an
    // [`Ownership::Owned`] `LocalWrite`. Assignment / read paths
    // for String today produce [`Ownership::Unowned`] (literals
    // are global pointers); concat / interpolation aren't lowered
    // yet. The promotion `LocalWrite` for `s` carries the only
    // Owned stamp in the function.
    let source = "
        fn taker(move s: String) -> String
          s
        end

        fn main
          taker(\"hello\")
        end
    ";

    let program = lower(&dedent(source));
    let taker = function(&program, "taker");

    let writes = local_writes(taker);
    let owned_writes: Vec<_> = writes
        .iter()
        .filter(|i| {
            matches!(
                i,
                IRInstruction::LocalWrite {
                    ownership: Ownership::Owned,
                    ..
                }
            )
        })
        .collect();

    assert_eq!(
        owned_writes.len(),
        1,
        "exactly one Owned LocalWrite expected (the `s` promotion); got {} in {writes:?}",
        owned_writes.len(),
    );
    let IRInstruction::LocalWrite { local, .. } = owned_writes[0] else {
        unreachable!()
    };

    let entry = taker.blocks.first().expect("entry block missing");
    let decl = entry
        .instructions
        .iter()
        .find_map(|i| match i {
            IRInstruction::LocalDecl {
                local: decl_local,
                ty,
            } if decl_local == local => Some(ty),
            _ => None,
        })
        .expect("LocalDecl for `s`'s slot must precede its Owned LocalWrite");

    assert_eq!(
        decl,
        &IRType::String,
        "`move s: String` slot's declared type must be IRType::String, got {decl:?}",
    );
}

#[test]
fn move_param_with_binary_type_stamps_owned_on_promotion() {
    // `Binary` joined the heap-type family in the strings/binary/bits
    // slice. `move b: Binary` must stamp Owned on the promotion
    // `LocalWrite`, mirroring `move s: String`. This pins the
    // `is_heap_type` extension to Binary.
    let source = "
        fn taker(move b: Binary) -> Binary
          b
        end

        fn main
          1
        end
    ";

    let program = lower(&dedent(source));
    let taker = function(&program, "taker");

    let writes = local_writes(taker);
    let owned_writes: Vec<_> = writes
        .iter()
        .filter(|i| {
            matches!(
                i,
                IRInstruction::LocalWrite {
                    ownership: Ownership::Owned,
                    ..
                }
            )
        })
        .collect();

    assert_eq!(
        owned_writes.len(),
        1,
        "exactly one Owned LocalWrite expected (the `b` promotion); got {} in {writes:?}",
        owned_writes.len(),
    );
    let IRInstruction::LocalWrite { local, .. } = owned_writes[0] else {
        unreachable!()
    };

    let entry = taker.blocks.first().expect("entry block missing");
    let decl = entry
        .instructions
        .iter()
        .find_map(|i| match i {
            IRInstruction::LocalDecl {
                local: decl_local,
                ty,
            } if decl_local == local => Some(ty),
            _ => None,
        })
        .expect("LocalDecl for `b`'s slot must precede its Owned LocalWrite");

    assert_eq!(
        decl,
        &IRType::Binary,
        "`move b: Binary` slot's declared type must be IRType::Binary, got {decl:?}",
    );
}

#[test]
fn move_param_with_bits_type_stamps_owned_on_promotion() {
    // Sibling of [`move_param_with_binary_type_stamps_owned_on_promotion`]:
    // `Bits` is the second new heap type added in this slice and must
    // stamp Owned on `move` promotion just like `String` and `Binary`.
    let source = "
        fn taker(move b: Bits) -> Bits
          b
        end

        fn main
          1
        end
    ";

    let program = lower(&dedent(source));
    let taker = function(&program, "taker");

    let writes = local_writes(taker);
    let owned_writes: Vec<_> = writes
        .iter()
        .filter(|i| {
            matches!(
                i,
                IRInstruction::LocalWrite {
                    ownership: Ownership::Owned,
                    ..
                }
            )
        })
        .collect();

    assert_eq!(
        owned_writes.len(),
        1,
        "exactly one Owned LocalWrite expected (the `b` promotion); got {} in {writes:?}",
        owned_writes.len(),
    );
    let IRInstruction::LocalWrite { local, .. } = owned_writes[0] else {
        unreachable!()
    };

    let entry = taker.blocks.first().expect("entry block missing");
    let decl = entry
        .instructions
        .iter()
        .find_map(|i| match i {
            IRInstruction::LocalDecl {
                local: decl_local,
                ty,
            } if decl_local == local => Some(ty),
            _ => None,
        })
        .expect("LocalDecl for `b`'s slot must precede its Owned LocalWrite");

    assert_eq!(
        decl,
        &IRType::Bits,
        "`move b: Bits` slot's declared type must be IRType::Bits, got {decl:?}",
    );
}

#[test]
fn match_binding_stamps_local_write_unowned() {
    let source = "
        fn main
          x = 7
          match x
            n -> n
          end
        end
    ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    assert_all_unowned(main, "match-arm pattern bind");
}

#[test]
fn struct_method_self_promotion_stamps_unowned() {
    // `self` on a default-receiver method lifts to `PassMode::Borrow`
    // (no `move` keyword), so promotion stamps Unowned regardless
    // of receiver type. This guards against accidentally treating
    // every `self` as Owned.
    let source = "
        struct Box
          fn read(self) -> Int
            1
          end
        end

        fn main
          1
        end
    ";

    let program = lower(&dedent(source));
    let read = function(&program, "Box.read");
    assert_all_unowned(read, "default-borrow self promotion");
}
