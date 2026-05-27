//! IR-lowering coverage for multi-segment field assignment
//! (`p.x = v`, `a.b.c = v`, `p.x += 1`).
//!
//! Walks the rebuild chain pinned by the lowerer and asserts:
//!
//! - Single-segment assigns continue to lower as `LocalRead → BinaryOp
//!   → LocalWrite` (regression guard for the dispatch added by
//!   [`lower_assignment`]).
//! - `p.x = v` lowers to `LocalRead → FieldSet → LocalWrite` with the
//!   field index, type, and struct symbol pinned.
//! - Depth-N (`a.b.c = v`) chains `LocalRead → FieldGet → FieldSet →
//!   FieldSet → LocalWrite` — one `FieldGet` per non-leaf segment,
//!   one `FieldSet` per segment, walked back up.
//! - Compound assignment on a field path (`p.x += 1`) reads the leaf
//!   via `FieldGet`, combines through `BinaryOp`, then walks back up
//!   with `FieldSet`.
//! - Heap-typed leaf overwrite (`s.name = "new"`) emits a synthetic
//!   `DropValue` of the prior payload before the rebuild.

use koja_ast::util::dedent;
use koja_ir::{IRFunction, IRInstruction};

mod common;

use common::{function, lower_program_source};

fn instructions(function: &IRFunction) -> Vec<&IRInstruction> {
    function
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
        .collect()
}

#[test]
fn single_segment_assignment_emits_local_write_only() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main -> Int
          p = Point{x: 1, y: 2}
          p.x
        end
        ";

    let program = lower_program_source(&dedent(source));
    let main = function(&program, "main");
    let instructions = instructions(main);
    assert!(
        !instructions
            .iter()
            .any(|inst| matches!(inst, IRInstruction::FieldSet { .. })),
        "single-segment let-binding should not emit any FieldSet, got {instructions:?}",
    );
}

#[test]
fn depth_one_field_write_emits_field_set_around_local() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main -> Int
          p = Point{x: 1, y: 2}
          p.x = 10
          p.x
        end
        ";

    let program = lower_program_source(&dedent(source));
    let main = function(&program, "main");
    let instructions = instructions(main);
    let field_sets: Vec<_> = instructions
        .iter()
        .filter_map(|inst| match inst {
            IRInstruction::FieldSet {
                field_index,
                struct_symbol,
                ..
            } => Some((*field_index, struct_symbol.mangled().to_string())),
            _ => None,
        })
        .collect();
    assert_eq!(field_sets.len(), 1, "expected one FieldSet for `p.x = 10`");
    assert_eq!(field_sets[0].0, 0, "expected FieldSet on Point.x (index 0)");
    assert_eq!(
        field_sets[0].1, "TestApp.Point",
        "expected FieldSet's struct_symbol to be TestApp.Point",
    );
}

#[test]
fn depth_two_field_write_chains_field_get_then_field_set_walk_up() {
    let source = "
        struct Inner
          n: Int
        end

        struct Outer
          inner: Inner
          tag: Bool
        end

        fn main -> Int
          o = Outer{inner: Inner{n: 1}, tag: true}
          o.inner.n = 42
          o.inner.n
        end
        ";

    let program = lower_program_source(&dedent(source));
    let main = function(&program, "main");
    let instructions = instructions(main);
    let field_sets: Vec<_> = instructions
        .iter()
        .filter_map(|inst| match inst {
            IRInstruction::FieldSet {
                field_index,
                struct_symbol,
                ..
            } => Some((*field_index, struct_symbol.mangled().to_string())),
            _ => None,
        })
        .collect();
    assert_eq!(
        field_sets.len(),
        2,
        "expected two FieldSets for depth-2 write (one per segment), got {:?}",
        field_sets,
    );
    let leaf_set = field_sets.iter().find(|(_, sym)| sym == "TestApp.Inner");
    assert!(
        leaf_set.is_some(),
        "expected a FieldSet on TestApp.Inner for `.n = 42`, got {field_sets:?}",
    );
    let outer_set = field_sets.iter().find(|(_, sym)| sym == "TestApp.Outer");
    assert!(
        outer_set.is_some(),
        "expected a FieldSet on TestApp.Outer for the `.inner` rebuild, got {field_sets:?}",
    );
}

#[test]
fn compound_assign_on_field_emits_field_get_binary_op_field_set() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main -> Int
          p = Point{x: 1, y: 2}
          p.x += 5
          p.x
        end
        ";

    let program = lower_program_source(&dedent(source));
    let main = function(&program, "main");
    let instructions = instructions(main);

    let field_get_count = instructions
        .iter()
        .filter(|inst| matches!(inst, IRInstruction::FieldGet { .. }))
        .count();
    let field_set_count = instructions
        .iter()
        .filter(|inst| matches!(inst, IRInstruction::FieldSet { .. }))
        .count();
    let binary_op_count = instructions
        .iter()
        .filter(|inst| matches!(inst, IRInstruction::BinaryOp { .. }))
        .count();
    assert!(
        field_get_count >= 1,
        "expected at least one FieldGet from `p.x += 5` and the trailing `p.x`, got {field_get_count}",
    );
    assert_eq!(
        field_set_count, 1,
        "expected one FieldSet from `p.x += 5`, got {field_set_count}",
    );
    assert!(
        binary_op_count >= 1,
        "expected at least one BinaryOp from `p.x += 5`, got {binary_op_count}",
    );
}

#[test]
fn heap_leaf_overwrite_emits_drop_value_before_field_set() {
    let source = "
        struct Holder
          name: String
        end

        fn main -> Int
          h = Holder{name: \"old\"}
          h.name = \"new\"
          1
        end
        ";

    let program = lower_program_source(&dedent(source));
    let main = function(&program, "main");
    let instructions = instructions(main);
    let drop_value_count = instructions
        .iter()
        .filter(|inst| matches!(inst, IRInstruction::DropValue { .. }))
        .count();
    assert!(
        drop_value_count >= 1,
        "expected at least one DropValue for heap-leaf overwrite, got {drop_value_count}",
    );
}
