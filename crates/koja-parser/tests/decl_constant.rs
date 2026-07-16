//! Coverage for top-level `const NAME [: T] = expr`.
//!
//! Pins:
//! - typed and untyped declarations
//! - lowercase (Ident) and PascalCase (TypeIdent) names both accepted
//! - annotations attach correctly

use koja_ast::ast::{ExprKind, TypeExpr, Visibility};

mod common;

use common::first_constant;

#[test]
fn priv_constant_records_private_visibility() {
    let c = first_constant(
        "
        priv const MAX: Int = 100
        ",
    );
    assert_eq!(c.visibility, Visibility::Private);
    assert_eq!(c.name, "MAX");
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
    assert_eq!(c.name, "MAX");
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
    assert_eq!(c.name, "Version");
}

#[test]
fn lowercase_constant_name() {
    let c = first_constant(
        "
        const max_size: Int = 1024
        ",
    );
    assert_eq!(c.name, "max_size");
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
