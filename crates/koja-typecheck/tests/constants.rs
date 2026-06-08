//! Package-level `const` lifting: literals, enums, structs, annotation
//! matching, interpolation / non-literal RHS rejection, duplicate names,
//! and immutability (no assigning to constants from function bodies).

use koja_ast::util::dedent;

mod common;

use common::{
    diagnostic_messages, typecheck_script as typecheck, typecheck_script_fail as typecheck_fail,
};

#[test]
fn primitive_string_and_struct_literal_constants_typecheck() {
    let source = "
        enum Direction
          North
        end

        struct Point
          x: Int
          y: Int
        end

        const N = 7
        const GREETING = \"hi\"
        const HEADING = Direction.North
        const ORIGIN = Point{x: 1, y: 2}

        N
        ";
    typecheck(&dedent(source));
}

#[test]
fn constant_annotation_mismatch_diagnoses() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        const P: String = Point{x: 0, y: 0}

        0
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| {
            m.contains("constant value type") && m.contains("does not match annotation")
        }),
        "expected annotation mismatch diagnostic, got {messages:?}",
    );
}

#[test]
fn non_literal_rhs_diagnoses() {
    let source = "
        const X = 1 + 1

        X
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| { m.contains("constant values are limited to literals") }),
        "expected non-literal RHS diagnostic, got {messages:?}",
    );
}

#[test]
fn interpolated_string_constant_diagnoses() {
    let source = "
        const S = \"a #{7} b\"

        S
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("interpolated strings are not constant-evaluable")),
        "expected interpolation diagnostic, got {messages:?}",
    );
}

#[test]
fn duplicate_constant_collides_like_other_globals() {
    let source = "
        const SAME = 1
        const SAME = 2

        SAME
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("already defined")),
        "expected duplicate definition diagnostic, got {messages:?}",
    );
}

#[test]
fn assignment_cannot_use_package_constant_as_lhs() {
    let source = "
        const PI = 3.14

        PI = 5.0
        0
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| { m.contains("package-level constants") && m.contains("immutable") }),
        "expected immutable-constant LHS diagnostic, got {messages:?}",
    );
}

#[test]
fn compound_assign_on_package_constant_diagnoses() {
    let source = "
        const STEP = 1

        STEP += 2
        0
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("immutable") && m.contains("STEP")),
        "expected compound-assign on constant diagnostic, got {messages:?}",
    );
}
