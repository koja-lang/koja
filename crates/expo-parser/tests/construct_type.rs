//! Coverage for type-construction expressions: struct literals,
//! enum variant construction (Unit, Tuple, Struct shapes), and
//! package-qualified forms (`Pkg.Type`, `Pkg.Type.Variant`).
//! Package names are PascalCase.

use expo_ast::ast::{EnumConstructionData, Expr, ExprKind, Item, Statement};
use expo_ast::util::dedent;

mod common;

use common::parse_clean;

fn first_expr(source: &str) -> Expr {
    let file = parse_clean(source);
    for item in file.items {
        if let Item::Function(f) = item {
            for stmt in f.body.unwrap_or_default() {
                match stmt {
                    Statement::Expr(e) | Statement::Assignment { value: e, .. } => return e,
                    _ => continue,
                }
            }
        }
    }
    panic!("no expression in parsed output");
}

#[test]
fn struct_construction_with_fields() {
    let src = dedent(
        "
        fn run
          p = Point { x: 1, y: 2 }
        end
        ",
    );
    let expr = first_expr(&src);
    match expr.kind {
        ExprKind::StructConstruction { type_path, fields } => {
            assert_eq!(type_path, vec!["Point"]);
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].name, "x");
            assert_eq!(fields[1].name, "y");
        }
        other => panic!("expected StructConstruction, got {other:?}"),
    }
}

#[test]
fn struct_construction_with_no_fields() {
    let src = dedent(
        "
        fn run
          o = Origin {}
        end
        ",
    );
    let expr = first_expr(&src);
    match expr.kind {
        ExprKind::StructConstruction { fields, .. } => assert!(fields.is_empty()),
        other => panic!("expected StructConstruction, got {other:?}"),
    }
}

#[test]
fn enum_construction_unit_variant() {
    let src = dedent(
        "
        fn run
          c = Color.Red
        end
        ",
    );
    let expr = first_expr(&src);
    match expr.kind {
        ExprKind::EnumConstruction {
            type_path,
            variant,
            data,
        } => {
            assert_eq!(type_path, vec!["Color"]);
            assert_eq!(variant, "Red");
            assert!(matches!(data, EnumConstructionData::Unit));
        }
        other => panic!("expected EnumConstruction, got {other:?}"),
    }
}

#[test]
fn enum_construction_tuple_variant() {
    let src = dedent(
        "
        fn run
          s = Shape.Rect(10, 20)
        end
        ",
    );
    let expr = first_expr(&src);
    match expr.kind {
        ExprKind::EnumConstruction { variant, data, .. } => {
            assert_eq!(variant, "Rect");
            match data {
                EnumConstructionData::Tuple(args) => assert_eq!(args.len(), 2),
                other => panic!("expected Tuple, got {other:?}"),
            }
        }
        other => panic!("expected EnumConstruction, got {other:?}"),
    }
}

#[test]
fn enum_construction_struct_variant() {
    let src = dedent(
        "
        fn run
          s = Event.Resize { width: 1, height: 2 }
        end
        ",
    );
    let expr = first_expr(&src);
    match expr.kind {
        ExprKind::EnumConstruction { variant, data, .. } => {
            assert_eq!(variant, "Resize");
            match data {
                EnumConstructionData::Struct(fields) => assert_eq!(fields.len(), 2),
                other => panic!("expected Struct, got {other:?}"),
            }
        }
        other => panic!("expected EnumConstruction, got {other:?}"),
    }
}

#[test]
fn package_dot_type_with_braces_parses_as_enum_construction() {
    // `Pkg.Point { x: 1, y: 2 }` is syntactically ambiguous between
    // a package-qualified struct construction and a `Pkg::Point`
    // enum-variant struct construction. The parser commits to the
    // enum shape unconditionally and lets later resolution decide;
    // this test pins that behavior so a refactor doesn't silently
    // flip it.
    let src = dedent(
        "
        fn run
          x = Pkg.Point { x: 1, y: 2 }
        end
        ",
    );
    let expr = first_expr(&src);
    match expr.kind {
        ExprKind::EnumConstruction {
            type_path,
            variant,
            data,
        } => {
            assert_eq!(type_path, vec!["Pkg"]);
            assert_eq!(variant, "Point");
            assert!(matches!(data, EnumConstructionData::Struct(_)));
        }
        other => panic!("expected EnumConstruction, got {other:?}"),
    }
}

#[test]
fn nested_package_qualified_enum_path() {
    let src = dedent(
        "
        fn run
          x = Pkg.Color.Red
        end
        ",
    );
    let expr = first_expr(&src);
    match expr.kind {
        ExprKind::EnumConstruction {
            type_path, variant, ..
        } => {
            assert_eq!(type_path, vec!["Pkg", "Color"]);
            assert_eq!(variant, "Red");
        }
        other => panic!("expected EnumConstruction, got {other:?}"),
    }
}
