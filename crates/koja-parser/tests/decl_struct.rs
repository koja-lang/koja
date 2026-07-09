//! Coverage for `struct` declarations.
//!
//! Pins:
//! - empty / single-field / multi-field structs
//! - default field values
//! - inline methods (`fn`, `priv fn`, `@annotation fn`)
//! - generic structs (`struct Box<T>`) and bounded generics
//!   (`struct Set<T: Eq & Hash>`)
//! - per-field type expressions resolve to the right shape

use koja_ast::ast::{EnumVariantData, Item, TypeExpr, Visibility};
use koja_ast::util::dedent;

mod common;

use common::parse_clean;

fn first_struct(source: &str) -> koja_ast::ast::StructDecl {
    let file = parse_clean(source);
    for item in file.items {
        if let Item::Struct(s) = item {
            return s;
        }
    }
    panic!("no struct in parsed output");
}

#[test]
fn priv_struct_records_private_visibility() {
    let src = dedent(
        "
        priv struct Internal
          slot: Int
        end
        ",
    );
    let s = first_struct(&src);
    assert_eq!(s.visibility, Visibility::Private);
    assert_eq!(s.name(), "Internal");
}

#[test]
fn struct_defaults_to_public_visibility() {
    let src = dedent(
        "
        struct Open
          slot: Int
        end
        ",
    );
    let s = first_struct(&src);
    assert_eq!(s.visibility, Visibility::Public);
}

#[test]
fn empty_struct() {
    let src = dedent(
        "
        struct Empty
        end
        ",
    );
    let s = first_struct(&src);
    assert_eq!(s.name(), "Empty");
    assert!(s.fields.is_empty());
    assert!(s.functions.is_empty());
    assert!(s.type_params.is_empty());
}

#[test]
fn struct_with_fields() {
    let src = dedent(
        "
        struct Point
          x: Int
          y: Int
        end
        ",
    );
    let s = first_struct(&src);
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
    let src = dedent(
        "
        struct Counter
          value: Int = 0
        end
        ",
    );
    let s = first_struct(&src);
    assert_eq!(s.fields.len(), 1);
    assert!(s.fields[0].default.is_some());
}

#[test]
fn struct_generic_type_params() {
    let src = dedent(
        "
        struct Box<T>
          inner: T
        end
        ",
    );
    let s = first_struct(&src);
    assert_eq!(s.type_params.len(), 1);
    assert_eq!(s.type_params[0].name, "T");
    assert!(s.type_params[0].bounds.is_empty());
}

#[test]
fn struct_type_param_bounds() {
    let src = dedent(
        "
        struct Set<T: Eq & Hash>
          slots: List<T>
        end
        ",
    );
    let s = first_struct(&src);
    assert_eq!(s.type_params.len(), 1);
    assert_eq!(s.type_params[0].bounds, vec!["Eq", "Hash"]);
}

#[test]
fn struct_with_inline_method() {
    let src = dedent(
        "
        struct Counter
          value: Int

          fn current() -> Int
            self.value
          end
        end
        ",
    );
    let s = first_struct(&src);
    assert_eq!(s.fields.len(), 1);
    assert_eq!(s.functions.len(), 1);
    assert_eq!(s.functions[0].name, "current");
    assert_eq!(s.functions[0].visibility, Visibility::Public);
}

#[test]
fn struct_with_priv_method() {
    let src = dedent(
        "
        struct Counter
          value: Int

          priv fn bump
            self.value + 1
          end
        end
        ",
    );
    let s = first_struct(&src);
    assert_eq!(s.functions.len(), 1);
    assert_eq!(s.functions[0].visibility, Visibility::Private);
}

#[test]
fn struct_with_annotated_method() {
    let src = dedent(
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
    let s = first_struct(&src);
    assert_eq!(s.functions.len(), 1);
    assert_eq!(s.functions[0].annotations.len(), 1);
    assert_eq!(s.functions[0].annotations[0].name, "doc");
}

#[test]
fn struct_methods_after_fields_interleave_correctly() {
    let src = dedent(
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
    let s = first_struct(&src);
    assert_eq!(s.fields.len(), 2);
    assert_eq!(s.functions.len(), 2);
    assert_eq!(s.functions[0].name, "sum");
    assert_eq!(s.functions[1].name, "diff");
}

#[test]
fn struct_field_with_generic_type() {
    let src = dedent(
        "
        struct Wrapper
          items: List<Int>
        end
        ",
    );
    let s = first_struct(&src);
    assert!(matches!(
        &s.fields[0].type_expr,
        TypeExpr::Generic { path, args, .. } if path == &["List"] && args.len() == 1
    ));
}

#[test]
fn struct_with_top_level_annotation() {
    let src = dedent(
        "
        @doc \"a point in the plane\"
        struct Point
          x: Int
          y: Int
        end
        ",
    );
    let s = first_struct(&src);
    assert_eq!(s.annotations.len(), 1);
    assert_eq!(s.annotations[0].name, "doc");
}

#[test]
fn empty_struct_variant_token_does_not_apply() {
    // Sanity: parse_struct_field is used for struct-variant enums too,
    // make sure the dedicated struct path produces a struct (not an
    // enum) for the same syntactic shape.
    let src = dedent(
        "
        struct Inline
          x: Int
        end
        ",
    );
    let s = first_struct(&src);
    assert!(!matches!(s.fields[0].type_expr, TypeExpr::Unit { .. }));
    let _unused = EnumVariantData::Unit;
}
