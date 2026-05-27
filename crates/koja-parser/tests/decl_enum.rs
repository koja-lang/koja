//! Coverage for `enum` declarations.
//!
//! Pins:
//! - the three variant shapes (Unit, Tuple, Struct)
//! - mixed variant kinds within one enum
//! - generic enums and inline methods (same shape as struct bodies)

use koja_ast::ast::{EnumVariantData, Item, TypeExpr};
use koja_ast::util::dedent;

mod common;

use common::parse_clean;

fn first_enum(source: &str) -> koja_ast::ast::EnumDecl {
    let file = parse_clean(source);
    for item in file.items {
        if let Item::Enum(e) = item {
            return e;
        }
    }
    panic!("no enum in parsed output");
}

#[test]
fn unit_variants_only() {
    let src = dedent(
        "
        enum Color
          Red
          Green
          Blue
        end
        ",
    );
    let e = first_enum(&src);
    assert_eq!(e.variants.len(), 3);
    for variant in &e.variants {
        assert!(matches!(variant.data, EnumVariantData::Unit));
    }
}

#[test]
fn tuple_variants() {
    let src = dedent(
        "
        enum Shape
          Circle(Int)
          Rect(Int, Int)
        end
        ",
    );
    let e = first_enum(&src);
    assert_eq!(e.variants.len(), 2);
    match &e.variants[0].data {
        EnumVariantData::Tuple(types) => assert_eq!(types.len(), 1),
        other => panic!("expected Tuple, got {other:?}"),
    }
    match &e.variants[1].data {
        EnumVariantData::Tuple(types) => assert_eq!(types.len(), 2),
        other => panic!("expected Tuple, got {other:?}"),
    }
}

#[test]
fn struct_variants() {
    let src = dedent(
        "
        enum Shape
          Rect { width: Int, height: Int }
        end
        ",
    );
    let e = first_enum(&src);
    match &e.variants[0].data {
        EnumVariantData::Struct(fields) => {
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].name, "width");
            assert_eq!(fields[1].name, "height");
        }
        other => panic!("expected Struct, got {other:?}"),
    }
}

#[test]
fn mixed_variant_kinds() {
    let src = dedent(
        "
        enum Event
          Quit
          Move(Int, Int)
          Resize { width: Int, height: Int }
        end
        ",
    );
    let e = first_enum(&src);
    assert_eq!(e.variants.len(), 3);
    assert!(matches!(e.variants[0].data, EnumVariantData::Unit));
    assert!(matches!(e.variants[1].data, EnumVariantData::Tuple(_)));
    assert!(matches!(e.variants[2].data, EnumVariantData::Struct(_)));
}

#[test]
fn generic_enum() {
    let src = dedent(
        "
        enum Option<T>
          None
          Some(T)
        end
        ",
    );
    let e = first_enum(&src);
    assert_eq!(e.type_params.len(), 1);
    assert_eq!(e.type_params[0].name, "T");
}

#[test]
fn enum_with_inline_methods() {
    let src = dedent(
        "
        enum Light
          Red
          Yellow
          Green

          fn next(self) -> Light
            Light.Red
          end
        end
        ",
    );
    let e = first_enum(&src);
    assert_eq!(e.variants.len(), 3);
    assert_eq!(e.functions.len(), 1);
    assert_eq!(e.functions[0].name, "next");
}

#[test]
fn enum_with_annotation() {
    let src = dedent(
        "
        @doc \"a value or absence\"
        enum Option<T>
          None
          Some(T)
        end
        ",
    );
    let e = first_enum(&src);
    assert_eq!(e.annotations.len(), 1);
    assert_eq!(e.annotations[0].name, "doc");
}

#[test]
fn tuple_variant_with_generic_inner_type() {
    let src = dedent(
        "
        enum Wrapper
          Box(List<Int>)
        end
        ",
    );
    let e = first_enum(&src);
    match &e.variants[0].data {
        EnumVariantData::Tuple(types) => {
            assert_eq!(types.len(), 1);
            assert!(matches!(
                &types[0],
                TypeExpr::Generic { path, .. } if path == &["List"]
            ));
        }
        other => panic!("expected Tuple, got {other:?}"),
    }
}
