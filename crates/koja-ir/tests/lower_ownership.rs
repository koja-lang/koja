//! Coverage for the ownership-stamping layer of lowering:
//!
//! - Every existing program shape stamps `Ownership::Unowned` on its
//!   `LocalWrite` instructions (the pipeline pre-filters non-heap types at
//!   the classifier — no expression today produces a heap-typed
//!   value, so every site is Unowned).
//! - Parameter promotion always stamps `Ownership::Unowned`: the
//!   `move` keyword is inert under value semantics, so a heap-typed
//!   parameter borrows (the caller retains and frees it) just like a
//!   stack-typed one.
//!
//! These tests sit at the IR-shape boundary — they don't exercise
//! the LLVM backend or eval. The smoke tests in
//! `tests/lower_drops.rs` cover end-to-end drop-pipeline shape.

use koja_ast::util::dedent;
use koja_ir::{IRFunction, IRInstruction, Ownership};

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
fn move_param_with_heap_type_stamps_unowned() {
    // The `move` keyword is inert under value semantics, so even a
    // heap-typed parameter promotes Unowned — the caller retains
    // ownership and frees its own slot. This guards against
    // resurrecting move-param ownership (which would re-introduce the
    // affine drop/borrow distinction).
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
    assert_all_unowned(taker, "move String param");
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
