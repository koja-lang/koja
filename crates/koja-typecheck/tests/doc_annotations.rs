//! `@doc` annotations are metadata consumed by `koja-doc` and LSP
//! hovers. The typechecker accepts them on public decls (it only
//! needed to stop rejecting them as a feature gap on the shapes that
//! historically diagnosed every annotation) and rejects them on
//! private decls, which never surface in generated docs. Other
//! annotation names (`@derive`, `@spec`, and friends) still raise
//! the existing message.
//!
//! Top-level functions and impl-block functions silently accept any
//! annotation today, so they aren't covered here. There's nothing to
//! relax. Trait-impl methods (`impl P for T`) carry their docs on the
//! parent protocol's method, so they're also not exercised.

use koja_ast::util::dedent;

mod common;

use common::{
    assert_script_fails_with, diagnostic_messages, typecheck_script as typecheck,
    typecheck_script_fail as typecheck_fail,
};

// ---------------------------------------------------------------------------
// Positive: `@doc "..."` accepted on every relaxed shape
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

#[test]
fn doc_string_on_constant_is_accepted() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        @doc \"The arithmetic origin.\"
        const ORIGIN = Point{x: 0, y: 0}

        ORIGIN.x
        ";
    typecheck(&dedent(source));
}

#[test]
fn doc_false_on_constant_is_accepted() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        @doc false
        const ORIGIN = Point{x: 0, y: 0}

        ORIGIN.x
        ";
    typecheck(&dedent(source));
}

// ---------------------------------------------------------------------------
// Positive: `@doc false` accepted on every relaxed shape
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
// Negative: `@doc` on a private function is a compile error
// ---------------------------------------------------------------------------

#[test]
fn doc_string_on_top_level_priv_fn_is_rejected() {
    let source = "
        @doc \"Internal helper.\"
        priv fn helper -> Int
          7
        end

        helper()
        ";

    assert_script_fails_with(
        source,
        &["`@doc` is not allowed on private function `helper`"],
    );
}

#[test]
fn doc_false_on_top_level_priv_fn_is_rejected() {
    let source = "
        @doc false
        priv fn helper -> Int
          7
        end

        helper()
        ";

    assert_script_fails_with(
        source,
        &["`@doc` is not allowed on private function `helper`"],
    );
}

#[test]
fn doc_string_on_type_body_priv_fn_is_rejected() {
    let source = "
        struct Point
          x: Int

          @doc \"Internal helper.\"
          priv fn shift(self) -> Int
            self.x + 1
          end
        end
        ";

    assert_script_fails_with(
        source,
        &["`@doc` is not allowed on private function `shift`"],
    );
}

#[test]
fn doc_string_on_public_top_level_fn_is_accepted() {
    let source = "
        @doc \"Adds one.\"
        fn bump(n: Int) -> Int
          n + 1
        end

        bump(1)
        ";
    typecheck(&dedent(source));
}

// ---------------------------------------------------------------------------
// Negative: `@doc` on other private decl kinds is a compile error
// ---------------------------------------------------------------------------

/// Assert `source` fails with the @doc-on-private message for
/// `kind_label` (e.g. "struct") on `name`.
fn assert_doc_on_private_rejected(source: &str, kind_label: &str, name: &str) {
    let needle = format!("`@doc` is not allowed on private {kind_label} `{name}`");
    assert_script_fails_with(source, &[&needle]);
}

#[test]
fn doc_string_on_priv_struct_is_rejected() {
    assert_doc_on_private_rejected(
        "
        @doc \"Internal state.\"
        priv struct Hidden
          slot: Int
        end
        ",
        "struct",
        "Hidden",
    );
}

#[test]
fn doc_string_on_priv_enum_is_rejected() {
    assert_doc_on_private_rejected(
        "
        @doc \"Internal modes.\"
        priv enum Mode
          Off
          On
        end
        ",
        "enum",
        "Mode",
    );
}

#[test]
fn doc_string_on_priv_protocol_is_rejected() {
    assert_doc_on_private_rejected(
        "
        @doc \"Internal contract.\"
        priv protocol Marked
          fn mark(self) -> Int
        end
        ",
        "protocol",
        "Marked",
    );
}

#[test]
fn doc_string_on_priv_constant_is_rejected() {
    assert_doc_on_private_rejected(
        "
        @doc \"Internal limit.\"
        priv const LIMIT: Int = 10

        LIMIT
        ",
        "constant",
        "LIMIT",
    );
}

#[test]
fn doc_string_on_priv_type_alias_is_rejected() {
    assert_doc_on_private_rejected(
        "
        struct Cat
          name: String
        end

        @doc \"Internal union.\"
        priv type Pet = Cat
        ",
        "type alias",
        "Pet",
    );
}

// ---------------------------------------------------------------------------
// Negative: non-`@doc` annotations still raise the feature-gap message
// ---------------------------------------------------------------------------

#[test]
fn non_doc_annotation_on_struct_still_diagnoses() {
    let source = "
        @derive
        struct Point
          x: Int
        end
        ";

    assert_script_fails_with(source, &["annotations on struct items", "@derive"]);
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

    assert_script_fails_with(source, &["annotations on enum items", "@derive"]);
}

#[test]
fn non_doc_annotation_on_protocol_still_diagnoses() {
    let source = "
        @derive
        protocol Show
          fn show(self) -> String
        end
        ";

    assert_script_fails_with(source, &["annotations on protocols", "@derive"]);
}

#[test]
fn non_doc_annotation_on_protocol_method_still_diagnoses() {
    let source = "
        protocol Show
          @derive
          fn show(self) -> String
        end
        ";

    assert_script_fails_with(source, &["annotations on protocol methods", "@derive"]);
}

#[test]
fn non_doc_annotation_on_constant_still_diagnoses() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        @derive
        const ORIGIN = Point{x: 0, y: 0}

        ORIGIN.x
        ";

    assert_script_fails_with(source, &["annotations on constant items", "@derive"]);
}

// ---------------------------------------------------------------------------
// Mixed: `@doc` paired with another annotation, the other one still fires
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

#[test]
fn doc_alongside_unsupported_annotation_on_constant_only_diagnoses_the_unsupported_one() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        @doc \"Origin.\"
        @derive
        const ORIGIN = Point{x: 0, y: 0}

        ORIGIN.x
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);

    let mentions_derive = messages
        .iter()
        .any(|m| m.contains("@derive") && m.contains("annotations on constant items"));
    assert!(
        mentions_derive,
        "expected `@derive` gap diagnostic on constants, got {messages:?}",
    );

    let mentions_doc = messages.iter().any(|m| m.contains("@doc"));
    assert!(
        !mentions_doc,
        "`@doc` must not raise a feature-gap diagnostic, got {messages:?}",
    );
}
