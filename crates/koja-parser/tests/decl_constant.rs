//! Coverage for top-level `const NAME [: T] = expr`.
//!
//! Pins:
//! - typed and untyped declarations
//! - lowercase (Ident) and PascalCase (TypeIdent) names both accepted
//! - annotations attach correctly

use koja_ast::ast::{Item, TypeExpr, Visibility};
use koja_ast::util::dedent;

mod common;

use common::parse_clean;

fn first_constant(source: &str) -> koja_ast::ast::Constant {
    let file = parse_clean(source);
    for item in file.items {
        if let Item::Constant(c) = item {
            return c;
        }
    }
    panic!("no constant in parsed output");
}

#[test]
fn priv_constant_records_private_visibility() {
    let src = dedent(
        "
        priv const MAX: Int = 100
        ",
    );
    let c = first_constant(&src);
    assert_eq!(c.visibility, Visibility::Private);
    assert_eq!(c.name, "MAX");
}

#[test]
fn constant_defaults_to_public_visibility() {
    let src = dedent(
        "
        const MAX: Int = 100
        ",
    );
    let c = first_constant(&src);
    assert_eq!(c.visibility, Visibility::Public);
}

#[test]
fn typed_constant() {
    let src = dedent(
        "
        const MAX: Int = 100
        ",
    );
    let c = first_constant(&src);
    assert_eq!(c.name, "MAX");
    assert!(
        matches!(c.type_annotation, Some(TypeExpr::Named { ref path, .. }) if path == &["Int"])
    );
}

#[test]
fn untyped_constant() {
    let src = dedent(
        "
        const PI = 3
        ",
    );
    let c = first_constant(&src);
    assert!(c.type_annotation.is_none());
}

#[test]
fn pascal_case_constant_name() {
    let src = dedent(
        "
        const Version = 1
        ",
    );
    let c = first_constant(&src);
    assert_eq!(c.name, "Version");
}

#[test]
fn lowercase_constant_name() {
    let src = dedent(
        "
        const max_size: Int = 1024
        ",
    );
    let c = first_constant(&src);
    assert_eq!(c.name, "max_size");
}

#[test]
fn annotated_constant() {
    let src = dedent(
        "
        @doc \"maximum payload bytes\"
        const MAX_BYTES: Int = 65535
        ",
    );
    let c = first_constant(&src);
    assert_eq!(c.annotations.len(), 1);
    assert_eq!(c.annotations[0].name, "doc");
}

#[test]
fn constant_with_compound_expression() {
    let src = dedent(
        "
        const TOTAL: Int = 10 + 20 * 3
        ",
    );
    let c = first_constant(&src);
    assert!(matches!(
        c.value.kind,
        koja_ast::ast::ExprKind::Binary { .. }
    ));
}
