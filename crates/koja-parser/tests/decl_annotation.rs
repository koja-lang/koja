//! Coverage for `@name [value]` annotations on declarations.
//!
//! Pins:
//! - `@name` bare (no value)
//! - `@name "string"`
//! - `@name` with a triple-quoted multiline payload
//! - `@name false`
//! - multiple stacked annotations
//! - annotations propagate to every supported declaration kind:
//!   struct / enum / fn / const / type alias / protocol
//! - the removed `@test` is reported once, with its replacement, and
//!   the declaration under it parses as plain

use koja_ast::ast::{AnnotationValue, Item};

mod common;

use common::{
    first_constant, first_enum, first_function, first_struct, parse_clean, parse_failing_with,
};

#[test]
fn bare_annotation_has_no_value() {
    let anns = first_function(
        "
        @experimental
        fn run
          1
        end
        ",
    )
    .annotations;
    assert_eq!(anns.len(), 1);
    assert_eq!(anns[0].name, "experimental");
    assert!(anns[0].value.is_none());
}

#[test]
fn string_annotation_carries_payload() {
    let anns = first_function(
        "
        @doc \"increments by one\"
        fn bump
          1
        end
        ",
    )
    .annotations;
    assert_eq!(anns.len(), 1);
    assert!(matches!(
        anns[0].value,
        Some(AnnotationValue::String(ref s)) if s == "increments by one"
    ));
}

#[test]
fn multiline_string_annotation() {
    let anns = first_function(
        "
        @doc \"\"\"
          multi
          line
        \"\"\"
        fn bump
          1
        end
        ",
    )
    .annotations;
    assert!(matches!(anns[0].value, Some(AnnotationValue::String(_))));
}

#[test]
fn false_annotation() {
    let anns = first_function(
        "
        @doc false
        fn skip
          1
        end
        ",
    )
    .annotations;
    assert_eq!(anns[0].name, "doc");
    assert!(matches!(anns[0].value, Some(AnnotationValue::False)));
}

#[test]
fn stacked_annotations() {
    let anns = first_function(
        "
        @extern \"C\"
        @link \"argon2\"
        fn argon2id_hash(t: UInt32) -> Int32
        ",
    )
    .annotations;
    assert_eq!(anns.len(), 2);
    assert_eq!(anns[0].name, "extern");
    assert_eq!(anns[1].name, "link");
}

#[test]
fn annotation_on_struct() {
    let s = first_struct(
        "
        @doc \"a point\"
        struct Point
          x: Int
        end
        ",
    );
    assert_eq!(s.annotations.len(), 1);
}

#[test]
fn annotation_on_enum() {
    let e = first_enum(
        "
        @doc \"sum type\"
        enum Either<L, R>
          Left(L)
          Right(R)
        end
        ",
    );
    assert_eq!(e.annotations.len(), 1);
}

#[test]
fn annotation_on_constant() {
    let c = first_constant(
        "
        @doc \"upper bound\"
        const LIMIT: Int = 100
        ",
    );
    assert_eq!(c.annotations.len(), 1);
}

#[test]
fn annotation_on_type_alias() {
    let file = parse_clean(
        "
        @doc \"unique id\"
        type UserId = Int
        ",
    );
    let a = match &file.items[0] {
        Item::TypeAlias(a) => a,
        other => panic!("expected type alias, got {other:?}"),
    };
    assert_eq!(a.annotations.len(), 1);
}

#[test]
fn annotation_followed_by_non_declaration_fails() {
    parse_failing_with(
        "
        @doc \"oops\"
        \"not a declaration\"
        ",
        &["annotation must be followed by a declaration"],
    );
}

#[test]
fn test_annotation_is_rejected_with_replacement_hint() {
    let result = parse_failing_with(
        "
        struct StackTest
          @test \"push then pop\"
          fn test_push_pop ! String
            ()
          end
        end

        @test
        fn top_level ! String
          ()
        end
        ",
        &["`@test` was removed in 0.20"],
    );
    assert_eq!(
        result.errors.len(),
        2,
        "one error per annotation and nothing else: {:#?}",
        result.errors
    );
    for error in &result.errors {
        let hint = error
            .hint
            .as_ref()
            .expect("removal diagnostic carries a hint");
        assert!(
            hint.contains("`test \"description\"` block"),
            "hint should name the replacement: {hint}"
        );
        assert!(error.fix.is_none(), "the removal carries no fix");
    }
    // The span starts at `@` and covers the payload, so the first
    // one ends past the `@test` keyword.
    assert_eq!(result.errors[0].span.start.line, 2);
    assert_eq!(result.errors[0].span.start.column, 3);
    assert_eq!(result.errors[0].span.end.line, 2);
    assert!(result.errors[0].span.end.column > 8);
    assert_eq!(result.errors[1].span.start.line, 8);
    assert_eq!(result.errors[1].span.start.column, 1);
}

#[test]
fn test_annotation_is_dropped_and_the_function_parses_plain() {
    let result = parse_failing_with(
        "
        struct StackTest
          @doc \"documented\"
          @test \"push then pop\"
          fn test_push_pop ! String
            ()
          end
        end
        ",
        &["`@test` was removed in 0.20"],
    );
    let Item::Struct(decl) = &result.ast.items[0] else {
        panic!("expected a struct item");
    };
    assert_eq!(decl.functions.len(), 1);
    let function = &decl.functions[0];
    assert_eq!(function.name.text, "test_push_pop");
    assert_eq!(function.annotations.len(), 1, "only `@doc` stays");
    assert_eq!(function.annotations[0].name, "doc");
    assert!(function.body.is_some());
}
