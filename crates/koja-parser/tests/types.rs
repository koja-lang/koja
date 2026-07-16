//! Coverage for the type-expression grammar.
//!
//! Pins every `TypeExpr` variant: Named (single + dotted),
//! Generic, Function, Union, Self_, Unit, and the `>>` ambiguity
//! when nested generics close two levels at once
//! (`Map<String, List<Int>>`).

use koja_ast::ast::TypeExpr;

mod common;

use common::{first_field_type, first_function, first_protocol};

#[test]
fn named_single_segment() {
    let ty = first_field_type(
        "
        struct S
          x: Int
        end
        ",
    );
    assert!(matches!(ty, TypeExpr::Named { path, .. } if path == vec!["Int"]));
}

#[test]
fn named_dotted_path() {
    // Package names are PascalCase, so a qualified type is two
    // `TypeIdent` segments stitched together with a dot.
    let ty = first_field_type(
        "
        struct S
          x: JSON.Decoder
        end
        ",
    );
    assert!(
        matches!(ty, TypeExpr::Named { path, .. } if path == vec!["JSON".to_string(), "Decoder".to_string()])
    );
}

#[test]
fn generic_single_arg() {
    let ty = first_field_type(
        "
        struct S
          x: List<Int>
        end
        ",
    );
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
    let ty = first_field_type(
        "
        struct S
          x: Map<String, Int>
        end
        ",
    );
    match ty {
        TypeExpr::Generic { args, .. } => assert_eq!(args.len(), 2),
        other => panic!("expected Generic, got {other:?}"),
    }
}

#[test]
fn nested_generics_close_with_gtgt() {
    let ty = first_field_type(
        "
        struct S
          x: Map<String, List<Int>>
        end
        ",
    );
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
    let ty = first_field_type(
        "
        struct S
          callback: fn() -> Int
        end
        ",
    );
    match ty {
        TypeExpr::Function { params, .. } => {
            assert!(params.is_empty());
        }
        other => panic!("expected Function, got {other:?}"),
    }
}

#[test]
fn function_type_with_params() {
    let ty = first_field_type(
        "
        struct S
          callback: fn(Int, String) -> Bool
        end
        ",
    );
    match ty {
        TypeExpr::Function { params, .. } => {
            assert_eq!(params.len(), 2);
        }
        other => panic!("expected Function, got {other:?}"),
    }
}

#[test]
fn union_type() {
    let ty = first_field_type(
        "
        struct S
          x: Int | String | Bool
        end
        ",
    );
    match ty {
        TypeExpr::Union { types, .. } => assert_eq!(types.len(), 3),
        other => panic!("expected Union, got {other:?}"),
    }
}

#[test]
fn self_type_inside_protocol() {
    let p = first_protocol(
        "
        protocol Show
          fn show(self) -> Self
        end
        ",
    );
    assert!(matches!(
        p.methods[0].return_type,
        Some(TypeExpr::Self_ { .. })
    ));
}

#[test]
fn unit_return_type() {
    let f = first_function(
        "
        fn run() -> ()
          ()
        end
        ",
    );
    assert!(matches!(f.return_type, Some(TypeExpr::Unit { .. })));
}

#[test]
fn package_qualified_generic_type() {
    let ty = first_field_type(
        "
        struct S
          x: Pkg.Container<Int>
        end
        ",
    );
    match ty {
        TypeExpr::Generic { path, .. } => {
            assert_eq!(path, vec!["Pkg".to_string(), "Container".to_string()]);
        }
        other => panic!("expected Generic, got {other:?}"),
    }
}
