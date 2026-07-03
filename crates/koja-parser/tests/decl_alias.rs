//! Coverage for `alias Pkg.Type` and `type Name = TypeExpr`
//! declarations. (Package names are PascalCase: `JSON`, `Net`,
//! `HTTP`, etc.)
//!
//! Pins:
//! - `alias` paths: short and deep PascalCase paths,
//!   `as` rename, and the trailing-TypeIdent requirement
//! - `type Name = TypeExpr` at the top level produces
//!   `Item::TypeAlias`

use koja_ast::ast::Item;
use koja_ast::util::dedent;

mod common;

use common::{assert_message_contains, parse_clean, parse_failing};

fn first_alias(source: &str) -> koja_ast::ast::AliasDecl {
    let file = parse_clean(source);
    for item in file.items {
        if let Item::Alias(a) = item {
            return a;
        }
    }
    panic!("no alias in parsed output");
}

#[test]
fn two_segment_alias() {
    let src = dedent(
        "
        alias JSON.Decoder
        ",
    );
    let a = first_alias(&src);
    assert_eq!(a.path, vec!["JSON", "Decoder"]);
    assert_eq!(a.local_name, "Decoder");
}

#[test]
fn alias_with_as_rename() {
    let src = dedent(
        "
        alias JSON.Decoder as Reader
        ",
    );
    let a = first_alias(&src);
    assert_eq!(a.path, vec!["JSON", "Decoder"]);
    assert_eq!(a.local_name, "Reader");
}

#[test]
fn alias_with_nested_type_segments() {
    let src = dedent(
        "
        alias HTTP.Request.Builder
        ",
    );
    let a = first_alias(&src);
    assert_eq!(a.path, vec!["HTTP", "Request", "Builder"]);
    assert_eq!(a.local_name, "Builder");
}

#[test]
fn alias_without_trailing_type_ident_fails() {
    let src = dedent(
        "
        alias Net.tcp
        ",
    );
    let result = parse_failing(&src);
    assert_message_contains(&result, "alias path must end with a type name (PascalCase)");
}

#[test]
fn top_level_type_alias() {
    let src = dedent(
        "
        type UserId = Int
        ",
    );
    let file = parse_clean(&src);
    let alias = match &file.items[0] {
        Item::TypeAlias(a) => a,
        other => panic!("expected TypeAlias, got {other:?}"),
    };
    assert_eq!(alias.name, "UserId");
}

#[test]
fn type_alias_with_generic_rhs() {
    let src = dedent(
        "
        type IntList = List<Int>
        ",
    );
    let file = parse_clean(&src);
    let alias = match &file.items[0] {
        Item::TypeAlias(a) => a,
        other => panic!("expected TypeAlias, got {other:?}"),
    };
    assert!(matches!(
        alias.type_expr,
        koja_ast::ast::TypeExpr::Generic { .. }
    ));
}
