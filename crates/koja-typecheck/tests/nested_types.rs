//! Typecheck coverage for nested type names (`Owner.Nested`): the
//! parser emits the same struct-shaped node for a nested-struct
//! construction (`Outer.Inner{...}`) and a struct-variant
//! construction (`Shape.Rect{...}`). Resolve disambiguates by kind,
//! rewriting the former to a `StructConstruction` / `Struct` pattern
//! in place. These tests pin that both readings keep working and that
//! `Owner.Nested` resolves in type position.

use koja_ast::util::dedent;

mod common;

use common::{assert_script_fails_with, typecheck_script as typecheck};

#[test]
fn constructs_and_matches_nested_struct() {
    typecheck(&dedent(
        "
        struct Outer
          tag: Int
        end

        struct Outer.Inner
          x: Int
        end

        fn make() -> Outer.Inner
          Outer.Inner{x: 5}
        end

        fn read(v: Outer.Inner) -> Int
          match v
            Outer.Inner{x: n} -> n
          end
        end

          read(make())
        ",
    ));
}

#[test]
fn struct_variant_construction_still_resolves() {
    typecheck(&dedent(
        "
        enum Shape
          Rect{w: Int, h: Int}
          Circle
        end

        fn area(s: Shape) -> Int
          match s
            Shape.Rect{w: w, h: h} -> w * h
            Shape.Circle -> 0
          end
        end

          area(Shape.Rect{w: 2, h: 3})
        ",
    ));
}

#[test]
fn generic_nested_struct_resolves_in_type_position() {
    typecheck(&dedent(
        "
        struct Box
          tag: Int
        end

        struct Box.Cell<T>
          value: T
        end

        fn wrap(value: Int) -> Box.Cell<Int>
          Box.Cell{value: value}
        end

        fn unwrap(cell: Box.Cell<Int>) -> Int
          match cell
            Box.Cell{value: v} -> v
          end
        end

          unwrap(wrap(7))
        ",
    ));
}

#[test]
fn deeply_nested_struct_constructs_and_matches() {
    typecheck(&dedent(
        "
        struct A
          tag: Int
        end

        struct A.B
          tag: Int
        end

        struct A.B.C
          x: Int
        end

        fn make() -> A.B.C
          A.B.C{x: 5}
        end

        fn read(v: A.B.C) -> Int
          match v
            A.B.C{x: n} -> n
          end
        end

          read(make())
        ",
    ));
}

#[test]
fn nested_type_under_protocol_owner_resolves() {
    typecheck(&dedent(
        "
        protocol Proc
          fn run(self)
        end

        struct Proc.ExitSignal
          code: Int
        end

        fn make() -> Proc.ExitSignal
          Proc.ExitSignal{code: 0}
        end

          make()
        ",
    ));
}

#[test]
fn alias_to_nested_type_resolves() {
    typecheck(&dedent(
        "
        struct Outer
          tag: Int
        end

        struct Outer.Inner
          x: Int
        end

        alias TestApp.Outer.Inner as Cell

        fn make() -> Cell
          Cell{x: 9}
        end

          make()
        ",
    ));
}

#[test]
fn nested_struct_unknown_field_diagnoses_against_nested_name() {
    assert_script_fails_with(
        "
        struct Outer
          tag: Int
        end

        struct Outer.Inner
          x: Int
        end

          Outer.Inner{y: 1}
        ",
        &["has no field `y`"],
    );
}
