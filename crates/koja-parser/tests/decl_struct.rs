//! Coverage for `struct` declarations.
//!
//! Pins:
//! - empty / single-field / multi-field structs
//! - default field values
//! - inline methods (`fn`, `priv fn`, `@annotation fn`)
//! - generic structs (`struct Box<T>`) and bounded generics
//!   (`struct Set<T: Eq & Hash>`)
//! - per-field type expressions resolve to the right shape
//! - lexically nested type declarations (`struct`/`enum` in the body)

use koja_ast::ast::{EnumVariantData, Item, StructDecl, TypeExpr, Visibility};

mod common;

use common::{first_struct, parse_failing_with};

#[test]
fn priv_struct_records_private_visibility() {
    let s = first_struct(
        "
        priv struct Internal
          slot: Int
        end
        ",
    );
    assert_eq!(s.visibility, Visibility::Private);
    assert_eq!(s.name(), "Internal");
}

#[test]
fn struct_defaults_to_public_visibility() {
    let s = first_struct(
        "
        struct Open
          slot: Int
        end
        ",
    );
    assert_eq!(s.visibility, Visibility::Public);
}

#[test]
fn empty_struct() {
    let s = first_struct(
        "
        struct Empty
        end
        ",
    );
    assert_eq!(s.name(), "Empty");
    assert!(s.fields.is_empty());
    assert!(s.functions.is_empty());
    assert!(s.type_params.is_empty());
}

#[test]
fn struct_with_fields() {
    let s = first_struct(
        "
        struct Point
          x: Int
          y: Int
        end
        ",
    );
    assert_eq!(s.fields.len(), 2);
    assert_eq!(s.fields[0].name, "x");
    assert_eq!(s.fields[1].name, "y");
    for field in &s.fields {
        assert!(matches!(&field.type_expr, TypeExpr::Named { path, .. } if path == &["Int"]));
        assert!(field.default.is_none());
    }
}

#[test]
fn struct_field_default_value() {
    let s = first_struct(
        "
        struct Counter
          value: Int = 0
        end
        ",
    );
    assert_eq!(s.fields.len(), 1);
    assert!(s.fields[0].default.is_some());
}

#[test]
fn struct_generic_type_params() {
    let s = first_struct(
        "
        struct Box<T>
          inner: T
        end
        ",
    );
    assert_eq!(s.type_params.len(), 1);
    assert_eq!(s.type_params[0].name, "T");
    assert!(s.type_params[0].bounds.is_empty());
}

#[test]
fn struct_type_param_bounds() {
    let s = first_struct(
        "
        struct Set<T: Eq & Hash>
          slots: List<T>
        end
        ",
    );
    assert_eq!(s.type_params.len(), 1);
    assert_eq!(s.type_params[0].bounds, vec!["Eq", "Hash"]);
}

#[test]
fn struct_with_inline_method() {
    let s = first_struct(
        "
        struct Counter
          value: Int

          fn current() -> Int
            self.value
          end
        end
        ",
    );
    assert_eq!(s.fields.len(), 1);
    assert_eq!(s.functions.len(), 1);
    assert_eq!(s.functions[0].name, "current");
    assert_eq!(s.functions[0].visibility, Visibility::Public);
}

#[test]
fn struct_with_priv_method() {
    let s = first_struct(
        "
        struct Counter
          value: Int

          priv fn bump
            self.value + 1
          end
        end
        ",
    );
    assert_eq!(s.functions.len(), 1);
    assert_eq!(s.functions[0].visibility, Visibility::Private);
}

#[test]
fn struct_with_annotated_method() {
    let s = first_struct(
        "
        struct Counter
          value: Int

          @doc \"increment by one\"
          fn bump
            self.value + 1
          end
        end
        ",
    );
    assert_eq!(s.functions.len(), 1);
    assert_eq!(s.functions[0].annotations.len(), 1);
    assert_eq!(s.functions[0].annotations[0].name, "doc");
}

#[test]
fn struct_methods_after_fields_interleave_correctly() {
    let s = first_struct(
        "
        struct Pair
          a: Int
          b: Int

          fn sum() -> Int
            self.a + self.b
          end

          fn diff() -> Int
            self.a - self.b
          end
        end
        ",
    );
    assert_eq!(s.fields.len(), 2);
    assert_eq!(s.functions.len(), 2);
    assert_eq!(s.functions[0].name, "sum");
    assert_eq!(s.functions[1].name, "diff");
}

#[test]
fn struct_field_with_generic_type() {
    let s = first_struct(
        "
        struct Wrapper
          items: List<Int>
        end
        ",
    );
    assert!(matches!(
        &s.fields[0].type_expr,
        TypeExpr::Generic { path, args, .. } if path == &["List"] && args.len() == 1
    ));
}

#[test]
fn struct_with_top_level_annotation() {
    let s = first_struct(
        "
        @doc \"a point in the plane\"
        struct Point
          x: Int
          y: Int
        end
        ",
    );
    assert_eq!(s.annotations.len(), 1);
    assert_eq!(s.annotations[0].name, "doc");
}

/// Extracts the nested item at `index`, asserting it is a struct.
fn nested_struct(owner: &StructDecl, index: usize) -> &StructDecl {
    match &owner.nested[index] {
        Item::Struct(s) => s,
        other => panic!("expected a nested struct, got {other:?}"),
    }
}

#[test]
fn struct_with_nested_struct() {
    let s = first_struct(
        "
        struct Owner
          x: Int

          struct Nested
            y: Int
          end
        end
        ",
    );
    assert_eq!(s.fields.len(), 1);
    assert_eq!(s.nested.len(), 1);
    let nested = nested_struct(&s, 0);
    assert_eq!(nested.path, vec!["Nested"]);
    assert_eq!(nested.fields.len(), 1);
    assert_eq!(nested.visibility, Visibility::Public);
}

#[test]
fn struct_with_nested_enum() {
    let s = first_struct(
        "
        struct Owner
          struct First
            a: Int
          end

          enum Kind
            Alpha
            Beta(Int)
          end
        end
        ",
    );
    assert_eq!(s.nested.len(), 2);
    assert!(matches!(&s.nested[0], Item::Struct(_)));
    let Item::Enum(kind) = &s.nested[1] else {
        panic!("expected a nested enum");
    };
    assert_eq!(kind.name(), "Kind");
    assert_eq!(kind.variants.len(), 2);
}

#[test]
fn priv_nested_struct_records_private_visibility() {
    let s = first_struct(
        "
        struct Owner
          priv struct Secret
            a: Int
          end
        end
        ",
    );
    assert_eq!(nested_struct(&s, 0).visibility, Visibility::Private);
}

#[test]
fn annotated_nested_struct() {
    let s = first_struct(
        "
        struct Owner
          @doc \"a helper\"
          struct Nested
            a: Int
          end
        end
        ",
    );
    let nested = nested_struct(&s, 0);
    assert_eq!(nested.annotations.len(), 1);
    assert_eq!(nested.annotations[0].name, "doc");
}

#[test]
fn nested_struct_depth_three() {
    let s = first_struct(
        "
        struct A
          struct B
            struct C
              x: Int
            end
          end
        end
        ",
    );
    let b = nested_struct(&s, 0);
    let c = nested_struct(b, 0);
    assert_eq!(c.path, vec!["C"]);
}

#[test]
fn nested_struct_interleaves_with_fields_and_methods() {
    let s = first_struct(
        "
        struct Owner
          x: Int

          struct Nested
            y: Int
          end

          fn double() -> Int
            self.x * 2
          end
        end
        ",
    );
    assert_eq!(s.fields.len(), 1);
    assert_eq!(s.nested.len(), 1);
    assert_eq!(s.functions.len(), 1);
}

#[test]
fn nested_struct_rejects_multi_segment_name() {
    parse_failing_with(
        "
        struct Owner
          struct Foo.Bar
            y: Int
          end
        end
        ",
        &["take a single name, found `Foo.Bar`"],
    );
}

#[test]
fn empty_struct_variant_token_does_not_apply() {
    // Sanity: parse_struct_field is used for struct-variant enums too,
    // make sure the dedicated struct path produces a struct (not an
    // enum) for the same syntactic shape.
    let s = first_struct(
        "
        struct Inline
          x: Int
        end
        ",
    );
    assert!(!matches!(s.fields[0].type_expr, TypeExpr::Unit { .. }));
    let _unused = EnumVariantData::Unit;
}
