use koja_ast::ast::{BinOp, Expr, ExprKind, ImplMember, Item, Literal, Statement, TypeExpr};
use koja_ast::util::dedent;
use koja_parser::ParseMode;
use koja_typecheck::CheckedProgram;

mod common;

use common::{PACKAGE, check_multi_file, typecheck_script as typecheck};

fn equality_body<'a>(checked: &'a CheckedProgram, type_name: &str) -> &'a Expr {
    let package = checked
        .packages
        .iter()
        .find(|package| package.package == PACKAGE)
        .expect("test package should be present");
    for item in package.files.iter().flat_map(|file| &file.items) {
        let Item::Impl(block) = item else {
            continue;
        };
        if type_expr_head(&block.trait_expr) != Some("Equality")
            || type_expr_head(&block.target) != Some(type_name)
        {
            continue;
        }
        for member in &block.members {
            let ImplMember::Function(function) = member else {
                continue;
            };
            if function.name != "eq" {
                continue;
            }
            let body = function.body.as_ref().expect("eq should have a body");
            let Statement::Expr(expression) = &body[0] else {
                panic!("eq body should contain one expression");
            };
            return expression;
        }
    }
    panic!("no synthesized `Equality for {type_name}` impl found");
}

fn type_expr_head(type_expr: &TypeExpr) -> Option<&str> {
    match type_expr {
        TypeExpr::Generic { path, .. } | TypeExpr::Named { path, .. } => {
            path.last().map(String::as_str)
        }
        _ => None,
    }
}

fn count_eq_calls(expression: &Expr) -> usize {
    match &expression.kind {
        ExprKind::Binary { left, right, .. } => count_eq_calls(left) + count_eq_calls(right),
        ExprKind::MethodCall { method, .. } if method == "eq" => 1,
        _ => 0,
    }
}

#[test]
fn enum_synthesizes_one_outer_arm_per_variant() {
    let source = "
        enum Shape
          Point
          Tagged(Int)
          Labeled{name: String}
        end

        1
        ";

    let checked = typecheck(&dedent(source));
    let ExprKind::Match { arms, .. } = &equality_body(&checked, "Shape").kind else {
        panic!("Shape.eq should use an outer match");
    };
    assert_eq!(arms.len(), 3);
}

#[test]
fn generic_struct_uses_universal_equality_dispatch() {
    let source = "
        struct Pair<A, B>
          first: A
          second: B
        end

        1
        ";

    let checked = typecheck(&dedent(source));
    assert_eq!(count_eq_calls(equality_body(&checked, "Pair")), 2);
}

#[test]
fn manual_impl_in_sibling_file_suppresses_synthesis() {
    let checked = check_multi_file(
        &[
            (
                "custom.koja",
                "
                struct Custom
                  value: Int
                end
                ",
            ),
            (
                "equality.koja",
                "
                impl Equality for Custom
                  fn eq(self, other: Custom) -> Bool
                    self.value == other.value
                  end
                end
                ",
            ),
        ],
        ParseMode::File,
    )
    .expect("manual Equality impl should typecheck");

    let count = checked
        .packages
        .iter()
        .flat_map(|package| &package.files)
        .flat_map(|file| &file.items)
        .filter(|item| {
            let Item::Impl(block) = item else {
                return false;
            };
            type_expr_head(&block.trait_expr) == Some("Equality")
                && type_expr_head(&block.target) == Some("Custom")
        })
        .count();
    assert_eq!(count, 1);
}

#[test]
fn opaque_fields_collapse_to_true() {
    let source = "
        struct Handle
          pointer: CPtr<Int32>
        end

        1
        ";

    let checked = typecheck(&dedent(source));
    assert!(matches!(
        equality_body(&checked, "Handle").kind,
        ExprKind::Literal {
            value: Literal::Bool(true)
        }
    ));
}

#[test]
fn struct_fields_synthesize_conjunction() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        1
        ";

    let checked = typecheck(&dedent(source));
    let body = equality_body(&checked, "Point");
    assert!(matches!(body.kind, ExprKind::Binary { op: BinOp::And, .. }));
    assert_eq!(count_eq_calls(body), 2);
}
