//! Surface-level coverage for multi-segment field assignment
//! (`p.x = v`, `a.b.c = v`, `p.x += 1`) and `self`-field mutation.
//!
//! Pinned shapes:
//!
//! - Depth-1 / depth-N field writes typecheck against the leaf field
//!   type (with type-args substituted at every step).
//! - Type mismatch on the rhs surfaces a clear diagnostic naming the
//!   offending lvalue and field type.
//! - Unknown field on a struct receiver diagnoses against the
//!   receiver name.
//! - `self.x = v` typechecks unconditionally: under value semantics
//!   `self` is an independent local value, so there is no
//!   borrowed/owned distinction to gate the mutation on.
//! - Compound assignment on a field (`p.x += 1`) requires the leaf
//!   field's type to be arithmetic.

use koja_ast::util::dedent;

mod common;

use common::{
    assert_script_fails_with, diagnostic_messages, typecheck_script as typecheck,
    typecheck_script_fail as typecheck_fail,
};

#[test]
fn depth_one_field_write_on_int_field_typechecks() {
    typecheck(&dedent(
        "
        struct Point
          x: Int
          y: Int
        end

        p = Point{x: 1, y: 2}
        p.x = 10
        p
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

        o = Outer{inner: Inner{n: 1}}
        o.inner.n = 42
        o
        ",
    ));
}

#[test]
fn field_write_type_mismatch_diagnoses_against_leaf_type() {
    assert_script_fails_with(
        "
        struct Point
          x: Int
          y: Int
        end

        p = Point{x: 1, y: 2}
        p.x = true
        p
        ",
        &["p.x", "Int", "Bool"],
    );
}

#[test]
fn field_write_on_unknown_field_diagnoses_against_struct_name() {
    assert_script_fails_with(
        "
        struct Point
          x: Int
        end

        p = Point{x: 1}
        p.z = 5
        p
        ",
        &["Point", "z"],
    );
}

#[test]
fn self_field_write_with_return_typechecks() {
    typecheck(&dedent(
        "
        struct Counter
          n: Int

          fn bump(self) -> Counter
            self.n = self.n + 1
            self
          end
        end

        c = Counter{n: 0}
        c.bump()
        ",
    ));
}

#[test]
fn self_field_write_typechecks() {
    // Under value semantics `self` field mutation typechecks on a
    // plain `self` receiver.
    typecheck(&dedent(
        "
        struct Counter
          n: Int

          fn bump(self)
            self.n = self.n + 1
          end
        end

        c = Counter{n: 0}
        c.bump()
        c
        ",
    ));
}

#[test]
fn compound_assign_on_field_requires_arithmetic_leaf() {
    let failure = typecheck_fail(&dedent(
        "
        struct Holder
          flag: Bool
        end

        h = Holder{flag: true}
        h.flag += true
        h
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
