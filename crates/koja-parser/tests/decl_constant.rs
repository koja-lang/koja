//! Coverage for `const NAME [: T] = expr`.
//!
//! Pins:
//! - typed and untyped declarations
//! - lowercase (Ident) and PascalCase (TypeIdent) names both accepted
//! - annotations attach correctly
//! - nested constants in both spellings, qualified at the top level
//!   (`const Span.ZERO = ...`) and lexically inside a struct, enum,
//!   or builtin body

use koja_ast::ast::{ExprKind, Item, TypeExpr, Visibility};

mod common;

use common::{first_builtin, first_constant, first_enum, first_struct, parse_failing_with};

#[test]
fn priv_constant_records_private_visibility() {
    let c = first_constant(
        "
        priv const MAX: Int = 100
        ",
    );
    assert_eq!(c.visibility, Visibility::Private);
    assert_eq!(*c.name(), "MAX");
}

#[test]
fn constant_defaults_to_public_visibility() {
    let c = first_constant(
        "
        const MAX: Int = 100
        ",
    );
    assert_eq!(c.visibility, Visibility::Public);
}

#[test]
fn typed_constant() {
    let c = first_constant(
        "
        const MAX: Int = 100
        ",
    );
    assert_eq!(*c.name(), "MAX");
    assert!(
        matches!(c.type_annotation, Some(TypeExpr::Named { ref path, .. }) if path == &["Int"])
    );
}

#[test]
fn untyped_constant() {
    let c = first_constant(
        "
        const PI = 3
        ",
    );
    assert!(c.type_annotation.is_none());
}

#[test]
fn pascal_case_constant_name() {
    let c = first_constant(
        "
        const Version = 1
        ",
    );
    assert_eq!(*c.name(), "Version");
}

#[test]
fn lowercase_constant_name() {
    let c = first_constant(
        "
        const max_size: Int = 1024
        ",
    );
    assert_eq!(*c.name(), "max_size");
}

#[test]
fn annotated_constant() {
    let c = first_constant(
        "
        @doc \"maximum payload bytes\"
        const MAX_BYTES: Int = 65535
        ",
    );
    assert_eq!(c.annotations.len(), 1);
    assert_eq!(c.annotations[0].name, "doc");
}

#[test]
fn constant_with_compound_expression() {
    let c = first_constant(
        "
        const TOTAL: Int = 10 + 20 * 3
        ",
    );
    assert!(matches!(c.value.kind, ExprKind::Binary { .. }));
}

#[test]
fn qualified_constant_records_full_path() {
    let c = first_constant(
        "
        const Span.ZERO = 0
        ",
    );
    assert_eq!(c.path, vec!["Span", "ZERO"]);
    assert_eq!(*c.name(), "ZERO");
    assert_eq!(c.owner_path().len(), 1);
    assert_eq!(c.owner_path()[0], "Span");
}

#[test]
fn qualified_constant_takes_a_lowercase_leaf() {
    let c = first_constant(
        "
        const Span.limit: Int = 100
        ",
    );
    assert_eq!(c.path, vec!["Span", "limit"]);
    assert!(c.type_annotation.is_some());
}

#[test]
fn struct_with_nested_constants() {
    let s = first_struct(
        "
        struct Span
          value: Int

          const ZERO = Span{value: 0}
          priv const limit = 100
        end
        ",
    );
    assert_eq!(s.nested.len(), 2);
    let Item::Constant(zero) = &s.nested[0] else {
        panic!("expected a nested constant");
    };
    assert_eq!(zero.path, vec!["ZERO"]);
    assert!(matches!(
        zero.value.kind,
        ExprKind::StructConstruction { .. }
    ));
    let Item::Constant(limit) = &s.nested[1] else {
        panic!("expected a nested constant");
    };
    assert_eq!(*limit.name(), "limit");
    assert_eq!(limit.visibility, Visibility::Private);
}

#[test]
fn enum_with_nested_constant() {
    let e = first_enum(
        "
        enum Color
          Red
          Green

          const DEFAULT = Color.Green
        end
        ",
    );
    assert_eq!(e.variants.len(), 2);
    let Item::Constant(default) = &e.nested[0] else {
        panic!("expected a nested constant");
    };
    assert_eq!(*default.name(), "DEFAULT");
}

#[test]
fn builtin_with_nested_constant() {
    let b = first_builtin(
        "
        builtin Int
          const MAX = 9223372036854775807

          fn abs(self) -> Int
            self
          end
        end
        ",
    );
    assert_eq!(b.functions.len(), 1);
    let Item::Constant(max) = &b.nested[0] else {
        panic!("expected a nested constant");
    };
    assert_eq!(*max.name(), "MAX");
}

#[test]
fn builtin_still_rejects_nested_types() {
    parse_failing_with(
        "
        builtin Int
          struct Inner
          end
        end
        ",
        &["expected a function or constant declaration in builtin block"],
    );
}

#[test]
fn nested_constant_rejects_multi_segment_name() {
    parse_failing_with(
        "
        struct Span
          const Foo.BAR = 1
        end
        ",
        &["nested constant declarations take a single name, found `Foo.BAR`"],
    );
}
