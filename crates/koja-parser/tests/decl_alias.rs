//! Coverage for `alias Pkg.Type` and `type Name = TypeExpr`
//! declarations. (Package names are PascalCase: `JSON`, `Net`,
//! `HTTP`, etc.)
//!
//! Pins:
//! - `alias` paths: short and deep PascalCase paths,
//!   `as` rename, and the trailing-TypeIdent requirement
//! - `type Name = TypeExpr` at the top level produces
//!   `Item::TypeAlias`

use koja_ast::ast::{Item, TypeExpr, Visibility};

mod common;

use common::{first_alias, parse_clean, parse_failing_with};

#[test]
fn two_segment_alias() {
    let a = first_alias(
        "
        alias JSON.Decoder
        ",
    );
    assert_eq!(a.path, vec!["JSON", "Decoder"]);
    assert_eq!(a.local_name, "Decoder");
}

#[test]
fn alias_with_as_rename() {
    let a = first_alias(
        "
        alias JSON.Decoder as Reader
        ",
    );
    assert_eq!(a.path, vec!["JSON", "Decoder"]);
    assert_eq!(a.local_name, "Reader");
}

#[test]
fn alias_with_nested_type_segments() {
    let a = first_alias(
        "
        alias HTTP.Request.Builder
        ",
    );
    assert_eq!(a.path, vec!["HTTP", "Request", "Builder"]);
    assert_eq!(a.local_name, "Builder");
}

#[test]
fn alias_without_trailing_type_ident_fails() {
    parse_failing_with(
        "
        alias Net.tcp
        ",
        &["alias path must end with a type name (PascalCase)"],
    );
}

#[test]
fn top_level_type_alias() {
    let file = parse_clean(
        "
        type UserId = Int
        ",
    );
    let alias = match &file.items[0] {
        Item::TypeAlias(a) => a,
        other => panic!("expected TypeAlias, got {other:?}"),
    };
    assert_eq!(alias.name, "UserId");
}

#[test]
fn priv_type_alias_records_private_visibility() {
    let file = parse_clean(
        "
        priv type UserId = Int
        ",
    );
    let alias = match &file.items[0] {
        Item::TypeAlias(a) => a,
        other => panic!("expected TypeAlias, got {other:?}"),
    };
    assert_eq!(alias.visibility, Visibility::Private);
    assert_eq!(alias.name, "UserId");
}

#[test]
fn type_alias_defaults_to_public_visibility() {
    let file = parse_clean(
        "
        type UserId = Int
        ",
    );
    let alias = match &file.items[0] {
        Item::TypeAlias(a) => a,
        other => panic!("expected TypeAlias, got {other:?}"),
    };
    assert_eq!(alias.visibility, Visibility::Public);
}

#[test]
fn type_alias_with_generic_rhs() {
    let file = parse_clean(
        "
        type IntList = List<Int>
        ",
    );
    let alias = match &file.items[0] {
        Item::TypeAlias(a) => a,
        other => panic!("expected TypeAlias, got {other:?}"),
    };
    assert!(matches!(alias.type_expr, TypeExpr::Generic { .. }));
}
