//! Coverage for `@name [value]` annotations on declarations.
//!
//! Pins:
//! - `@name` bare (no value)
//! - `@name "string"`
//! - `@name """multiline"""`
//! - `@name false`
//! - multiple stacked annotations
//! - annotations propagate to every supported declaration kind:
//!   struct / enum / fn / const / type alias / protocol

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
