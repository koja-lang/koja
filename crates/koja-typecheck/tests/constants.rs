//! Package-level `const` lifting: literals, enums, structs, annotation
//! matching, interpolation / non-literal RHS rejection, duplicate names,
//! immutability (no assigning to constants from function bodies), and
//! package-qualified reads across package boundaries.

use koja_ast::util::dedent;
use koja_parser::ParseMode;
use koja_typecheck::{CheckFailure, CheckedProgram};

mod common;

use common::{
    PACKAGE, assert_script_fails_with, check_packages, diagnostic_messages,
    typecheck_script as typecheck, warning_messages,
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

// Package-qualified reads. `Lib.MAX` parses as a unit enum
// construction and `Lib.default_size` as a field access, so both
// surface shapes are covered.

const LIB_CONSTANTS: &str = "
    const MAX = 100
    const default_size = 25
    priv const HIDDEN = 7

    @deprecated \"Use MAX instead.\"
    const OLD_MAX = 50

    fn helper() -> Int
      1
    end

    priv fn hidden_helper() -> Int
      2
    end

    fn identity<T>(x: T) -> T
      x
    end

    struct Widget
      size: Int
    end
    ";

fn check_lib_and_app(app: &str) -> Result<CheckedProgram, CheckFailure> {
    check_packages(
        &[
            ("Lib", "lib.koja", LIB_CONSTANTS),
            (PACKAGE, "main.kojs", app),
        ],
        ParseMode::Script,
    )
}

fn assert_app_fails_with(app: &str, needle: &str) {
    let failure = check_lib_and_app(app).expect_err("expected a diagnostic");
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains(needle)),
        "expected `{needle}`, got {messages:?}",
    );
}

#[test]
fn public_constants_readable_cross_package() {
    check_lib_and_app(
        "
        total: Int = Lib.MAX + Lib.default_size
        total.print()
        ",
    )
    .expect("public cross-package constant reads should succeed");
}

#[test]
fn qualified_read_within_own_package() {
    typecheck(&dedent(
        "
        const MAX = 100

        TestApp.MAX.print()
        ",
    ));
}

#[test]
fn priv_constant_rejected_cross_package() {
    assert_app_fails_with(
        "Lib.HIDDEN.print()",
        "private constant `Lib.HIDDEN` cannot be referenced from package `TestApp`",
    );
}

#[test]
fn deprecated_constant_warns_at_qualified_read() {
    let checked =
        check_lib_and_app("Lib.OLD_MAX.print()").expect("deprecated reads still typecheck");
    let warnings = warning_messages(&checked);
    assert!(
        warnings
            .iter()
            .any(|m| m.contains("`OLD_MAX` is deprecated")),
        "expected a deprecation warning, got {warnings:?}",
    );
}

#[test]
fn unknown_member_in_known_package_diagnoses() {
    assert_app_fails_with(
        "Lib.MAXX.print()",
        "package `Lib` has no constant or function `MAXX`",
    );
}

#[test]
fn function_value_readable_cross_package() {
    check_lib_and_app(
        "
        f = &Lib.helper/0
        result: Int = f()
        result.print()
        ",
    )
    .expect("cross-package function values should typecheck");
}

#[test]
fn priv_function_value_rejected_cross_package() {
    assert_app_fails_with(
        "f = &Lib.hidden_helper/0\nf().print()",
        "private function `Lib.hidden_helper` cannot be referenced from package `TestApp`",
    );
}

#[test]
fn generic_function_value_diagnoses() {
    assert_app_fails_with(
        "f = &Lib.identity/1\nf(1).print()",
        "cannot reference generic function `Lib.identity` directly",
    );
}

#[test]
fn type_member_read_diagnoses() {
    assert_app_fails_with("x = Lib.Widget\n0", "`Lib.Widget` is a struct, not a value");
}

#[test]
fn global_constants_resolve_bare() {
    typecheck(&dedent(
        "
        out: Fd = STDOUT
        out.print()
        ",
    ));
}

#[test]
fn package_constant_shadows_global_constant() {
    typecheck(&dedent(
        "
        const STDOUT = 1

        shadowed: Int = STDOUT
        shadowed.print()
        ",
    ));
}

#[test]
fn enum_in_scope_wins_over_package_prefix() {
    check_lib_and_app(
        "
        enum Lib
          MAX
        end

        heading: Lib = Lib.MAX
        heading.print()
        ",
    )
    .expect("a type named like a package takes precedence over the package");
}
