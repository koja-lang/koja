//! Surface-level coverage for multi-segment field assignment
//! (`p.x = v`, `a.b.c = v`, `p.x += 1`) and the `move self`
//! mutation rule.
//!
//! Pinned shapes:
//!
//! - Depth-1 / depth-N field writes typecheck against the leaf field
//!   type (with type-args substituted at every step).
//! - Type mismatch on the rhs surfaces a clear diagnostic naming the
//!   offending lvalue and field type.
//! - Unknown field on a struct receiver diagnoses against the
//!   receiver name.
//! - `self.x = v` typechecks under `move self` and is rejected under
//!   borrowed `self` (mirrors v1's `expo-typecheck::stmt` rule).
//! - Compound assignment on a field (`p.x += 1`) requires the leaf
//!   field's type to be arithmetic.

use expo_ast::util::dedent;

mod common;

use common::{
    diagnostic_messages, typecheck_file as typecheck, typecheck_file_fail as typecheck_fail,
};

#[test]
fn depth_one_field_write_on_int_field_typechecks() {
    typecheck(&dedent(
        "
        struct Point
          x: Int
          y: Int
        end

        fn main
          p = Point{x: 1, y: 2}
          p.x = 10
          p
        end
        ",
    ));
}

#[test]
fn depth_two_field_write_typechecks_through_nested_struct() {
    typecheck(&dedent(
        "
        struct Inner
          n: Int
        end

        struct Outer
          inner: Inner
        end

        fn main
          o = Outer{inner: Inner{n: 1}}
          o.inner.n = 42
          o
        end
        ",
    ));
}

#[test]
fn field_write_type_mismatch_diagnoses_against_leaf_type() {
    let failure = typecheck_fail(&dedent(
        "
        struct Point
          x: Int
          y: Int
        end

        fn main
          p = Point{x: 1, y: 2}
          p.x = true
          p
        end
        ",
    ));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("p.x") && m.contains("Int") && m.contains("Bool")),
        "expected leaf-type mismatch diagnostic naming the field path, got {messages:?}",
    );
}

#[test]
fn field_write_on_unknown_field_diagnoses_against_struct_name() {
    let failure = typecheck_fail(&dedent(
        "
        struct Point
          x: Int
        end

        fn main
          p = Point{x: 1}
          p.z = 5
          p
        end
        ",
    ));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("Point") && m.contains("z")),
        "expected unknown-field diagnostic naming `Point` and `z`, got {messages:?}",
    );
}

#[test]
fn self_field_write_typechecks_under_move_self() {
    typecheck(&dedent(
        "
        struct Counter
          n: Int

          fn bump(move self) -> Counter
            self.n = self.n + 1
            self
          end
        end

        fn main
          c = Counter{n: 0}
          c.bump()
        end
        ",
    ));
}

#[test]
fn self_field_write_under_borrowed_self_diagnoses() {
    let failure = typecheck_fail(&dedent(
        "
        struct Counter
          n: Int

          fn bump(self)
            self.n = self.n + 1
          end
        end

        fn main
          c = Counter{n: 0}
          c.bump()
          c
        end
        ",
    ));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("self") && m.contains("borrowed")),
        "expected `self` mutation rejection mentioning the borrow, got {messages:?}",
    );
}

#[test]
fn compound_assign_on_field_requires_arithmetic_leaf() {
    let failure = typecheck_fail(&dedent(
        "
        struct Holder
          flag: Bool
        end

        fn main
          h = Holder{flag: true}
          h.flag += true
          h
        end
        ",
    ));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("Int") || m.contains("Float") || m.contains("Bool")),
        "expected arithmetic-leaf diagnostic for compound assign on Bool field, got {messages:?}",
    );
}
