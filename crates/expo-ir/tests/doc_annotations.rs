//! End-to-end coverage that `@doc` flows through the IR lowering
//! sub-pass without raising the per-decl annotation feature-gap
//! diagnostic in [`crate::lower::structs::has_feature_gap`] /
//! [`crate::lower::enums::has_feature_gap`].
//!
//! The IR-side rejection runs as defense-in-depth (typecheck rejects
//! the same shapes earlier), so this test mainly pins behavioral
//! parity between the two rejection sites. Negative coverage for
//! non-`@doc` annotations lives in
//! `expo-alpha-typecheck/tests/doc_annotations.rs` — those programs
//! never reach the IR pass.

use expo_ast::util::dedent;

mod common;

use common::{PACKAGE, lower_program_source};

#[test]
fn doc_annotated_struct_lowers_through_ir() {
    let source = "
        @doc \"A 2D point.\"
        struct Point
          x: Int
          y: Int
        end

        fn main -> Int
          Point{x: 1, y: 2}.x
        end
        ";

    let program = lower_program_source(&dedent(source));
    let mangled = format!("{PACKAGE}.Point");
    assert!(
        program.struct_decl(&mangled).is_some(),
        "expected `Point` struct decl in lowered IR after `@doc` relaxation",
    );
}

#[test]
fn doc_annotated_enum_lowers_through_ir() {
    let source = "
        @doc \"Primary colors.\"
        enum Color
          Red
          Green
          Blue
        end

        fn main -> Int
          c: Color = Color.Red
          1
        end
        ";

    let program = lower_program_source(&dedent(source));
    let mangled = format!("{PACKAGE}.Color");
    assert!(
        program.enum_decl(&mangled).is_some(),
        "expected `Color` enum decl in lowered IR after `@doc` relaxation",
    );
}
