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
use koja_ast::util::dedent;

mod common;

use common::{assert_message_contains, parse_clean, parse_failing};

fn first_function_annotations(source: &str) -> Vec<koja_ast::ast::Annotation> {
    let file = parse_clean(source);
    for item in file.items {
        if let Item::Function(f) = item {
            return f.annotations;
        }
    }
    panic!("no function in parsed output");
}

#[test]
fn bare_annotation_has_no_value() {
    let src = dedent(
        "
        @experimental
        fn run
          1
        end
        ",
    );
    let anns = first_function_annotations(&src);
    assert_eq!(anns.len(), 1);
    assert_eq!(anns[0].name, "experimental");
    assert!(anns[0].value.is_none());
}

#[test]
fn string_annotation_carries_payload() {
    let src = dedent(
        "
        @doc \"increments by one\"
        fn bump
          1
        end
        ",
    );
    let anns = first_function_annotations(&src);
    assert_eq!(anns.len(), 1);
    assert!(matches!(
        anns[0].value,
        Some(AnnotationValue::String(ref s)) if s == "increments by one"
    ));
}

#[test]
fn multiline_string_annotation() {
    let src = dedent(
        "
        @doc \"\"\"
          multi
          line
        \"\"\"
        fn bump
          1
        end
        ",
    );
    let anns = first_function_annotations(&src);
    assert!(matches!(anns[0].value, Some(AnnotationValue::String(_))));
}

#[test]
fn false_annotation() {
    let src = dedent(
        "
        @doc false
        fn skip
          1
        end
        ",
    );
    let anns = first_function_annotations(&src);
    assert_eq!(anns[0].name, "doc");
    assert!(matches!(anns[0].value, Some(AnnotationValue::False)));
}

#[test]
fn stacked_annotations() {
    let src = dedent(
        "
        @extern \"C\"
        @link \"argon2\"
        fn argon2id_hash(t: UInt32) -> Int32
        ",
    );
    let anns = first_function_annotations(&src);
    assert_eq!(anns.len(), 2);
    assert_eq!(anns[0].name, "extern");
    assert_eq!(anns[1].name, "link");
}

#[test]
fn annotation_on_struct() {
    let src = dedent(
        "
        @doc \"a point\"
        struct Point
          x: Int
        end
        ",
    );
    let file = parse_clean(&src);
    let s = match &file.items[0] {
        Item::Struct(s) => s,
        other => panic!("expected struct, got {other:?}"),
    };
    assert_eq!(s.annotations.len(), 1);
}

#[test]
fn annotation_on_enum() {
    let src = dedent(
        "
        @doc \"sum type\"
        enum Either<L, R>
          Left(L)
          Right(R)
        end
        ",
    );
    let file = parse_clean(&src);
    let e = match &file.items[0] {
        Item::Enum(e) => e,
        other => panic!("expected enum, got {other:?}"),
    };
    assert_eq!(e.annotations.len(), 1);
}

#[test]
fn annotation_on_constant() {
    let src = dedent(
        "
        @doc \"upper bound\"
        const LIMIT: Int = 100
        ",
    );
    let file = parse_clean(&src);
    let c = match &file.items[0] {
        Item::Constant(c) => c,
        other => panic!("expected constant, got {other:?}"),
    };
    assert_eq!(c.annotations.len(), 1);
}

#[test]
fn annotation_on_type_alias() {
    let src = dedent(
        "
        @doc \"unique id\"
        type UserId = Int
        ",
    );
    let file = parse_clean(&src);
    let a = match &file.items[0] {
        Item::TypeAlias(a) => a,
        other => panic!("expected type alias, got {other:?}"),
    };
    assert_eq!(a.annotations.len(), 1);
}

#[test]
fn annotation_followed_by_non_declaration_fails() {
    let src = dedent(
        "
        @doc \"oops\"
        \"not a declaration\"
        ",
    );
    let result = parse_failing(&src);
    assert_message_contains(&result, "annotation must be followed by a declaration");
}
