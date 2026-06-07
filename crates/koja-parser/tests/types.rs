//! Coverage for the type-expression grammar.
//!
//! Pins every `TypeExpr` variant: Named (single + dotted),
//! Generic, Function, Union, Self_, Unit, and the `>>` ambiguity
//! when nested generics close two levels at once
//! (`Map<String, List<Int>>`).

use koja_ast::ast::{Item, TypeExpr};
use koja_ast::util::dedent;

mod common;

use common::parse_clean;

fn first_field_type(source: &str) -> TypeExpr {
    let file = parse_clean(source);
    for item in file.items {
        if let Item::Struct(s) = item {
            return s.fields.into_iter().next().unwrap().type_expr;
        }
    }
    panic!("no struct in parsed output");
}

#[test]
fn named_single_segment() {
    let src = dedent(
        "
        struct S
          x: Int
        end
        ",
    );
    let ty = first_field_type(&src);
    assert!(matches!(ty, TypeExpr::Named { path, .. } if path == vec!["Int"]));
}

#[test]
fn named_dotted_path() {
    // Package names are PascalCase, so a qualified type is two
    // `TypeIdent` segments stitched together with a dot.
    let src = dedent(
        "
        struct S
          x: JSON.Decoder
        end
        ",
    );
    let ty = first_field_type(&src);
    assert!(
        matches!(ty, TypeExpr::Named { path, .. } if path == vec!["JSON".to_string(), "Decoder".to_string()])
    );
}

#[test]
fn generic_single_arg() {
    let src = dedent(
        "
        struct S
          x: List<Int>
        end
        ",
    );
    let ty = first_field_type(&src);
    match ty {
        TypeExpr::Generic { path, args, .. } => {
            assert_eq!(path, vec!["List"]);
            assert_eq!(args.len(), 1);
        }
        other => panic!("expected Generic, got {other:?}"),
    }
}

#[test]
fn generic_multiple_args() {
    let src = dedent(
        "
        struct S
          x: Map<String, Int>
        end
        ",
    );
    let ty = first_field_type(&src);
    match ty {
        TypeExpr::Generic { args, .. } => assert_eq!(args.len(), 2),
        other => panic!("expected Generic, got {other:?}"),
    }
}

#[test]
fn nested_generics_close_with_gtgt() {
    let src = dedent(
        "
        struct S
          x: Map<String, List<Int>>
        end
        ",
    );
    let ty = first_field_type(&src);
    match ty {
        TypeExpr::Generic { args, .. } => {
            assert_eq!(args.len(), 2);
            assert!(matches!(args[1], TypeExpr::Generic { .. }));
        }
        other => panic!("expected Generic, got {other:?}"),
    }
}

#[test]
fn function_type_no_params() {
    let src = dedent(
        "
        struct S
          callback: fn() -> Int
        end
        ",
    );
    let ty = first_field_type(&src);
    match ty {
        TypeExpr::Function { params, .. } => {
            assert!(params.is_empty());
        }
        other => panic!("expected Function, got {other:?}"),
    }
}

#[test]
fn function_type_with_params() {
    let src = dedent(
        "
        struct S
          callback: fn(Int, String) -> Bool
        end
        ",
    );
    let ty = first_field_type(&src);
    match ty {
        TypeExpr::Function { params, .. } => {
            assert_eq!(params.len(), 2);
        }
        other => panic!("expected Function, got {other:?}"),
    }
}

#[test]
fn union_type() {
    let src = dedent(
        "
        struct S
          x: Int | String | Bool
        end
        ",
    );
    let ty = first_field_type(&src);
    match ty {
        TypeExpr::Union { types, .. } => assert_eq!(types.len(), 3),
        other => panic!("expected Union, got {other:?}"),
    }
}

#[test]
fn self_type_inside_protocol() {
    let src = dedent(
        "
        protocol Show
          fn show(self) -> Self
        end
        ",
    );
    let file = parse_clean(&src);
    let p = match &file.items[0] {
        Item::Protocol(p) => p,
        other => panic!("expected protocol, got {other:?}"),
    };
    assert!(matches!(
        p.methods[0].return_type,
        Some(TypeExpr::Self_ { .. })
    ));
}

#[test]
fn unit_return_type() {
    let src = dedent(
        "
        fn run() -> ()
          ()
        end
        ",
    );
    let file = parse_clean(&src);
    let f = match &file.items[0] {
        Item::Function(f) => f,
        other => panic!("expected function, got {other:?}"),
    };
    assert!(matches!(f.return_type, Some(TypeExpr::Unit { .. })));
}

#[test]
fn package_qualified_generic_type() {
    let src = dedent(
        "
        struct S
          x: Pkg.Container<Int>
        end
        ",
    );
    let ty = first_field_type(&src);
    match ty {
        TypeExpr::Generic { path, .. } => {
            assert_eq!(path, vec!["Pkg".to_string(), "Container".to_string()]);
        }
        other => panic!("expected Generic, got {other:?}"),
    }
}
