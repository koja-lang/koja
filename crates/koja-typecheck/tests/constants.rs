//! Package-level `const` lifting: literals, enums, structs, annotation
//! matching, interpolation / non-literal RHS rejection, duplicate names,
//! and immutability (no assigning to constants from function bodies).

use koja_ast::util::dedent;

mod common;

use common::{assert_script_fails_with, typecheck_script as typecheck};

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

    assert_script_fails_with(
        source,
        &["constant value type", "does not match annotation"],
    );
}

#[test]
fn non_literal_rhs_diagnoses() {
    let source = "
        const X = 1 + 1

        X
        ";

    assert_script_fails_with(source, &["constant values are limited to literals"]);
}

#[test]
fn interpolated_string_constant_diagnoses() {
    let source = "
        const S = \"a #{7} b\"

        S
        ";

    assert_script_fails_with(source, &["interpolated strings are not constant-evaluable"]);
}

#[test]
fn binary_literal_constants_typecheck() {
    let source = "
        const SYNC: Binary = <<0x53::8, 4::32>>
        const GREETING = <<\"hi\", 0::8>>
        const FLAGS: Bits = <<5::3>>

        SYNC
        ";
    typecheck(&dedent(source));
}

#[test]
fn binary_constant_with_non_literal_segment_diagnoses() {
    let source = "
        const TAG = 5
        const FRAME: Binary = <<TAG::8>>

        FRAME
        ";

    assert_script_fails_with(
        source,
        &["binary segment values in a constant must be literals"],
    );
}

#[test]
fn binary_constant_segment_out_of_range_diagnoses() {
    let source = "
        const FRAME: Binary = <<300::8>>

        FRAME
        ";

    assert_script_fails_with(source, &["does not fit in 8 unsigned bits"]);
}

#[test]
fn binary_constant_segment_kind_mismatch_diagnoses() {
    // A float-annotated segment folds a float literal's bits and
    // nothing else. resolve_segment already rejects `1.5::8` on its
    // own, so the int-into-float direction is the interesting one.
    let source = "
        const FRAME: Binary = <<7: Float32>>

        FRAME
        ";

    assert_script_fails_with(source, &["does not match the segment's declared shape"]);
}

#[test]
fn bits_valued_binary_constant_with_binary_annotation_diagnoses() {
    let source = "
        const FLAGS: Binary = <<5::3>>

        FLAGS
        ";

    assert_script_fails_with(
        source,
        &["constant value type", "does not match annotation"],
    );
}

#[test]
fn duplicate_constant_collides_like_other_globals() {
    let source = "
        const SAME = 1
        const SAME = 2

        SAME
        ";

    assert_script_fails_with(source, &["already defined"]);
}

#[test]
fn assignment_cannot_use_package_constant_as_lhs() {
    let source = "
        const PI = 3.14

        PI = 5.0
        0
        ";

    assert_script_fails_with(source, &["package-level constants", "immutable"]);
}

#[test]
fn compound_assign_on_package_constant_diagnoses() {
    let source = "
        const STEP = 1

        STEP += 2
        0
        ";

    assert_script_fails_with(source, &["immutable", "STEP"]);
}
