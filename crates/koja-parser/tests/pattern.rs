//! Coverage for every `Pattern` variant.
//!
//! Patterns appear in `match` arms, `for` loops, and destructuring
//! assignments. Each test exercises a representative source shape
//! and asserts the resulting AST variant; combined this pins which
//! syntactic shape maps to which pattern node.

use koja_ast::ast::{Expr, ExprKind, Item, Literal, Pattern, Statement};
use koja_ast::util::dedent;

mod common;

use common::{assert_message_contains, parse_clean, parse_failing};

fn first_match(source: &str) -> Vec<koja_ast::ast::MatchArm> {
    let file = parse_clean(source);
    for item in file.items {
        if let Item::Function(f) = item {
            for stmt in f.body.unwrap_or_default() {
                if let Statement::Expr(Expr {
                    kind: ExprKind::Match { arms, .. },
                    ..
                })
                | Statement::Assignment {
                    value:
                        Expr {
                            kind: ExprKind::Match { arms, .. },
                            ..
                        },
                    ..
                } = stmt
                {
                    return arms;
                }
            }
        }
    }
    panic!("no match expression in parsed output");
}

#[test]
fn wildcard_pattern() {
    let src = dedent(
        "
        fn run
          match x
            _ -> 0
          end
        end
        ",
    );
    let arms = first_match(&src);
    assert!(matches!(arms[0].pattern, Pattern::Wildcard { .. }));
}

#[test]
fn binding_pattern() {
    let src = dedent(
        "
        fn run
          match x
            n -> n
          end
        end
        ",
    );
    let arms = first_match(&src);
    assert!(matches!(arms[0].pattern, Pattern::Binding { ref name, .. } if name == "n"));
}

#[test]
fn literal_int_pattern() {
    let src = dedent(
        "
        fn run
          match x
            42 -> 1
            _ -> 0
          end
        end
        ",
    );
    let arms = first_match(&src);
    assert!(matches!(arms[0].pattern, Pattern::Literal { .. }));
}

#[test]
fn literal_string_pattern() {
    let src = dedent(
        "
        fn run
          match x
            \"hello\" -> 1
            _ -> 0
          end
        end
        ",
    );
    let arms = first_match(&src);
    assert!(matches!(arms[0].pattern, Pattern::Literal { .. }));
}

/// Multiline string patterns share `parse_string_expr`'s dedent
/// semantics (closing-quote column is the indent oracle, leading /
/// trailing newlines stripped), so a pattern and an identical
/// expression always agree on the matched text.
#[test]
fn literal_multiline_string_pattern() {
    let src = dedent(
        r#"
        fn run
          match x
            """
            hello
            world
            """ -> 1
            _ -> 0
          end
        end
        "#,
    );
    let arms = first_match(&src);
    let Pattern::Literal {
        value: Literal::String(text),
        ..
    } = &arms[0].pattern
    else {
        panic!("expected string literal pattern, got {:?}", arms[0].pattern);
    };
    assert_eq!(text, "hello\nworld");
}

#[test]
fn string_pattern_with_interpolation_is_diagnosed() {
    let src = dedent(
        "
        fn run
          match x
            \"a#{y}b\" -> 1
            _ -> 0
          end
        end
        ",
    );
    let result = parse_failing(&src);
    assert_message_contains(&result, "string patterns cannot contain");
}

#[test]
fn literal_bool_pattern() {
    let src = dedent(
        "
        fn run
          match x
            true -> 1
            false -> 0
          end
        end
        ",
    );
    let arms = first_match(&src);
    assert!(matches!(arms[0].pattern, Pattern::Literal { .. }));
    assert!(matches!(arms[1].pattern, Pattern::Literal { .. }));
}

#[test]
fn or_pattern() {
    let src = dedent(
        "
        fn run
          match x
            1 | 2 | 3 -> \"small\"
            _ -> \"big\"
          end
        end
        ",
    );
    let arms = first_match(&src);
    match &arms[0].pattern {
        Pattern::Or { patterns, .. } => assert_eq!(patterns.len(), 3),
        other => panic!("expected Or, got {other:?}"),
    }
}

#[test]
fn list_pattern() {
    let src = dedent(
        "
        fn run
          match items
            [a, b] -> a
            _ -> 0
          end
        end
        ",
    );
    let arms = first_match(&src);
    match &arms[0].pattern {
        Pattern::List { elements, .. } => assert_eq!(elements.len(), 2),
        other => panic!("expected List, got {other:?}"),
    }
}

#[test]
fn enum_unit_variant_pattern() {
    let src = dedent(
        "
        fn run
          match c
            Color.Red -> 1
            _ -> 0
          end
        end
        ",
    );
    let arms = first_match(&src);
    match &arms[0].pattern {
        Pattern::EnumUnit {
            type_path, variant, ..
        } => {
            assert_eq!(type_path, &vec!["Color".to_string()]);
            assert_eq!(variant, "Red");
        }
        other => panic!("expected EnumUnit, got {other:?}"),
    }
}

#[test]
fn enum_tuple_variant_pattern() {
    let src = dedent(
        "
        fn run
          match o
            Option.Some(x) -> x
            Option.None -> 0
          end
        end
        ",
    );
    let arms = first_match(&src);
    match &arms[0].pattern {
        Pattern::EnumTuple {
            variant, elements, ..
        } => {
            assert_eq!(variant, "Some");
            assert_eq!(elements.len(), 1);
        }
        other => panic!("expected EnumTuple, got {other:?}"),
    }
}

#[test]
fn enum_struct_variant_pattern() {
    let src = dedent(
        "
        fn run
          match s
            Shape.Rect { width: w, height: h } -> w * h
            _ -> 0
          end
        end
        ",
    );
    let arms = first_match(&src);
    match &arms[0].pattern {
        Pattern::EnumStruct {
            variant, fields, ..
        } => {
            assert_eq!(variant, "Rect");
            assert_eq!(fields.len(), 2);
        }
        other => panic!("expected EnumStruct, got {other:?}"),
    }
}

#[test]
fn constructor_shorthand_pattern() {
    let src = dedent(
        "
        fn run
          match r
            Ok(x) -> x
            Err(_) -> 0
          end
        end
        ",
    );
    let arms = first_match(&src);
    match &arms[0].pattern {
        Pattern::Constructor { name, elements, .. } => {
            assert_eq!(name, "Ok");
            assert_eq!(elements.len(), 1);
        }
        other => panic!("expected Constructor, got {other:?}"),
    }
}

#[test]
fn struct_destructure_pattern() {
    let src = dedent(
        "
        fn run
          match p
            Point { x: a, y: b } -> a + b
            _ -> 0
          end
        end
        ",
    );
    let arms = first_match(&src);
    match &arms[0].pattern {
        Pattern::Struct {
            type_path, fields, ..
        } => {
            assert_eq!(type_path, &vec!["Point".to_string()]);
            assert_eq!(fields.len(), 2);
        }
        other => panic!("expected Struct, got {other:?}"),
    }
}

#[test]
fn binary_pattern() {
    let src = dedent(
        "
        fn run
          match data
            <<header::8, payload::16 big>> -> header
            <<>> -> 0
          end
        end
        ",
    );
    let arms = first_match(&src);
    match &arms[0].pattern {
        Pattern::Binary { segments, .. } => assert_eq!(segments.len(), 2),
        other => panic!("expected Binary, got {other:?}"),
    }
}

#[test]
fn package_qualified_enum_unit_pattern() {
    let src = dedent(
        "
        fn run
          match c
            Pkg.Color.Red -> 1
            _ -> 0
          end
        end
        ",
    );
    let arms = first_match(&src);
    match &arms[0].pattern {
        Pattern::EnumUnit {
            type_path, variant, ..
        } => {
            assert_eq!(type_path, &vec!["Pkg".to_string(), "Color".to_string()]);
            assert_eq!(variant, "Red");
        }
        other => panic!("expected EnumUnit, got {other:?}"),
    }
}
