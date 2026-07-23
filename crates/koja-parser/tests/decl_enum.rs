//! Coverage for `enum` declarations.
//!
//! Pins:
//! - the three variant shapes (Unit, Tuple, Struct)
//! - mixed variant kinds within one enum
//! - generic enums and inline methods (same shape as struct bodies)
//! - lexically nested type declarations (`struct`/`enum` in the body)

use koja_ast::ast::{EnumVariantData, Item, TypeExpr, Visibility};

mod common;

use common::first_enum;

#[test]
fn priv_enum_records_private_visibility() {
    let e = first_enum(
        "
        priv enum Mode
          On
          Off
        end
        ",
    );
    assert_eq!(e.visibility, Visibility::Private);
    assert_eq!(e.name(), "Mode");
}

#[test]
fn enum_defaults_to_public_visibility() {
    let e = first_enum(
        "
        enum Mode
          On
          Off
        end
        ",
    );
    assert_eq!(e.visibility, Visibility::Public);
}

#[test]
fn unit_variants_only() {
    let e = first_enum(
        "
        enum Color
          Red
          Green
          Blue
        end
        ",
    );
    assert_eq!(e.variants.len(), 3);
    for variant in &e.variants {
        assert!(matches!(variant.data, EnumVariantData::Unit));
    }
}

#[test]
fn tuple_variants() {
    let e = first_enum(
        "
        enum Shape
          Circle(Int)
          Rect(Int, Int)
        end
        ",
    );
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
    let e = first_enum(
        "
        enum Shape
          Rect { width: Int, height: Int }
        end
        ",
    );
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
    let e = first_enum(
        "
        enum Event
          Quit
          Move(Int, Int)
          Resize { width: Int, height: Int }
        end
        ",
    );
    assert_eq!(e.variants.len(), 3);
    assert!(matches!(e.variants[0].data, EnumVariantData::Unit));
    assert!(matches!(e.variants[1].data, EnumVariantData::Tuple(_)));
    assert!(matches!(e.variants[2].data, EnumVariantData::Struct(_)));
}

#[test]
fn generic_enum() {
    let e = first_enum(
        "
        enum Option<T>
          None
          Some(T)
        end
        ",
    );
    assert_eq!(e.type_params.len(), 1);
    assert_eq!(e.type_params[0].name, "T");
}

#[test]
fn enum_with_inline_methods() {
    let e = first_enum(
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
    assert_eq!(e.variants.len(), 3);
    assert_eq!(e.functions.len(), 1);
    assert_eq!(e.functions[0].name, "next");
}

#[test]
fn enum_with_annotation() {
    let e = first_enum(
        "
        @doc \"a value or absence\"
        enum Option<T>
          None
          Some(T)
        end
        ",
    );
    assert_eq!(e.annotations.len(), 1);
    assert_eq!(e.annotations[0].name, "doc");
}

#[test]
fn tuple_variant_with_generic_inner_type() {
    let e = first_enum(
        "
        enum Wrapper
          Box(List<Int>)
        end
        ",
    );
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

#[test]
fn enum_with_nested_struct() {
    let e = first_enum(
        "
        enum Tree
          Leaf
          Node(Int)

          struct Meta
            depth: Int
          end
        end
        ",
    );
    assert_eq!(e.variants.len(), 2);
    assert_eq!(e.nested.len(), 1);
    let Item::Struct(meta) = &e.nested[0] else {
        panic!("expected a nested struct");
    };
    assert_eq!(meta.path, vec!["Meta"]);
}

#[test]
fn enum_with_nested_enum() {
    let e = first_enum(
        "
        enum Outer
          A

          priv enum Inner
            B
          end
        end
        ",
    );
    let Item::Enum(inner) = &e.nested[0] else {
        panic!("expected a nested enum");
    };
    assert_eq!(inner.name(), "Inner");
    assert_eq!(inner.visibility, Visibility::Private);
}
