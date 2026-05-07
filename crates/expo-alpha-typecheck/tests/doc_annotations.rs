//! `@doc` annotations are pure metadata for `expo-doc` / `expo-fmt`.
//! Alpha typecheck neither honors nor enforces them; it only needs to
//! stop rejecting them as a feature gap on the decl shapes that
//! historically diagnosed every annotation. Other annotation names
//! (`@derive`, `@spec`, …) still raise the existing message.
//!
//! Top-level functions and impl-block functions silently accept any
//! annotation today, so they aren't covered here — there's nothing to
//! relax. Trait-impl methods (`impl P for T`) carry their docs on the
//! parent protocol's method, so they're also not exercised.

use expo_ast::util::dedent;

mod common;

use common::{
    diagnostic_messages, typecheck_file as typecheck, typecheck_file_fail as typecheck_fail,
};

// ---------------------------------------------------------------------------
// Positive — `@doc "..."` accepted on every relaxed shape
// ---------------------------------------------------------------------------

#[test]
fn doc_string_on_struct_is_accepted() {
    let source = "
        @doc \"A 2D point.\"
        struct Point
          x: Int
          y: Int
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn doc_string_on_enum_is_accepted() {
    let source = "
        @doc \"Primary colors.\"
        enum Color
          Red
          Green
          Blue
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn doc_string_on_protocol_is_accepted() {
    let source = "
        @doc \"Things that can render to a string.\"
        protocol Show
          fn show(self) -> String
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn doc_string_on_protocol_method_is_accepted() {
    let source = "
        protocol Show
          @doc \"Render `self` as a String.\"
          fn show(self) -> String
        end
        ";
    typecheck(&dedent(source));
}

// ---------------------------------------------------------------------------
// Positive — `@doc false` accepted on every relaxed shape
// ---------------------------------------------------------------------------

#[test]
fn doc_false_on_struct_is_accepted() {
    let source = "
        @doc false
        struct Hidden
          slot: Int
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn doc_false_on_enum_is_accepted() {
    let source = "
        @doc false
        enum Hidden
          One
          Two
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn doc_false_on_protocol_is_accepted() {
    let source = "
        @doc false
        protocol Hidden
          fn op(self) -> Int
        end
        ";
    typecheck(&dedent(source));
}

#[test]
fn doc_false_on_protocol_method_is_accepted() {
    let source = "
        protocol Visible
          @doc false
          fn private_helper(self) -> Int
          fn public_op(self) -> Int
        end
        ";
    typecheck(&dedent(source));
}

// ---------------------------------------------------------------------------
// Negative — non-`@doc` annotations still raise the feature-gap message
// ---------------------------------------------------------------------------

#[test]
fn non_doc_annotation_on_struct_still_diagnoses() {
    let source = "
        @derive
        struct Point
          x: Int
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("annotations on struct items") && m.contains("@derive")),
        "expected struct annotation gap to still fire on `@derive`, got {messages:?}",
    );
}

#[test]
fn non_doc_annotation_on_enum_still_diagnoses() {
    let source = "
        @derive
        enum Color
          Red
          Green
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("annotations on enum items") && m.contains("@derive")),
        "expected enum annotation gap to still fire on `@derive`, got {messages:?}",
    );
}

#[test]
fn non_doc_annotation_on_protocol_still_diagnoses() {
    let source = "
        @derive
        protocol Show
          fn show(self) -> String
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("annotations on protocols") && m.contains("@derive")),
        "expected protocol annotation gap to still fire on `@derive`, got {messages:?}",
    );
}

#[test]
fn non_doc_annotation_on_protocol_method_still_diagnoses() {
    let source = "
        protocol Show
          @derive
          fn show(self) -> String
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("annotations on protocol methods") && m.contains("@derive")),
        "expected protocol-method annotation gap to still fire on `@derive`, got {messages:?}",
    );
}

// ---------------------------------------------------------------------------
// Mixed — `@doc` paired with another annotation: the other one still fires
// ---------------------------------------------------------------------------

#[test]
fn doc_alongside_unsupported_annotation_only_diagnoses_the_unsupported_one() {
    let source = "
        @doc \"Primary colors.\"
        @derive
        enum Color
          Red
          Green
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);

    let mentions_derive = messages
        .iter()
        .any(|m| m.contains("@derive") && m.contains("annotations on enum items"));
    assert!(
        mentions_derive,
        "expected `@derive` gap diagnostic, got {messages:?}",
    );

    let mentions_doc = messages.iter().any(|m| m.contains("@doc"));
    assert!(
        !mentions_doc,
        "`@doc` must not raise a feature-gap diagnostic, got {messages:?}",
    );
}
