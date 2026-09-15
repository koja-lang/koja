//! Coverage for the `Name` at reference sites. Every identifier token
//! the parser reads at a use site keeps its own span, so a rename or
//! a diagnostic can point at one segment of a path.
//!
//! Pins:
//! - a qualified type in a field annotation has one span per segment
//! - a generic type path has one span per segment
//! - a method chain records the span of each method name
//! - a function reference has one span per path segment
//! - enum patterns, constructor patterns, and struct patterns record
//!   the type path and variant spans
//! - a struct construction and an enum construction record their
//!   type path spans
//! - params, closure params, type params, bindings, and assignment
//!   targets record the name span
//! - an alias records one span per path segment and the local name

use koja_ast::ast::{
    ClosureParam, ExprKind, File, Item, Name, Param, Pattern, Statement, TypeExpr,
};
use koja_ast::span::Span;
use koja_ast::util::dedent;
use koja_parser::{ParseMode, parse};

/// The source text under `span`.
fn text_at<'a>(source: &'a str, span: &Span) -> &'a str {
    &source[span.start.offset as usize..span.end.offset as usize]
}

fn assert_name(source: &str, name: &Name, expected: &str) {
    assert_eq!(name, expected);
    assert_eq!(text_at(source, &name.span), expected);
}

fn assert_path(source: &str, path: &[Name], expected: &[&str]) {
    assert_eq!(path.len(), expected.len(), "{path:?}");
    for (name, expected) in path.iter().zip(expected) {
        assert_name(source, name, expected);
    }
}

fn file(source: &str) -> (String, File) {
    let source = dedent(source);
    let result = parse(&source, ParseMode::File);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    (source, result.ast)
}

fn first_function_body(source: &str) -> (String, Vec<Statement>) {
    let (source, ast) = file(source);
    let body = ast
        .items
        .into_iter()
        .find_map(|item| match item {
            Item::Function(f) => f.body,
            _ => None,
        })
        .expect("expected a function with a body");
    (source, body)
}

fn expr_of(statement: &Statement) -> &koja_ast::ast::Expr {
    match statement {
        Statement::Expr(e) | Statement::Assignment { value: e, .. } => e,
        other => panic!("expected an expression statement, got {other:?}"),
    }
}

#[test]
fn qualified_type_has_one_span_per_segment() {
    let (source, ast) = file(
        "
        struct Worker
          signal: Process.ExitSignal
        end
        ",
    );
    let Item::Struct(s) = &ast.items[0] else {
        panic!("expected a struct")
    };
    let TypeExpr::Named { path, span, .. } = &s.fields[0].type_expr else {
        panic!("expected a named type")
    };
    assert_path(&source, path, &["Process", "ExitSignal"]);
    assert_eq!(text_at(&source, span), "Process.ExitSignal");
    assert_name(&source, &s.fields[0].name, "signal");
}

#[test]
fn generic_type_path_has_one_span_per_segment() {
    let (source, ast) = file(
        "
        struct Queue
          items: List<Job.Item>
        end
        ",
    );
    let Item::Struct(s) = &ast.items[0] else {
        panic!("expected a struct")
    };
    let TypeExpr::Generic { path, args, .. } = &s.fields[0].type_expr else {
        panic!("expected a generic type")
    };
    assert_path(&source, path, &["List"]);
    let TypeExpr::Named { path, .. } = &args[0] else {
        panic!("expected a named type argument")
    };
    assert_path(&source, path, &["Job", "Item"]);
}

#[test]
fn method_chain_records_each_method_name_span() {
    let (source, body) = first_function_body(
        "
        fn run(items: List<Int>) -> Int
          items.filter(x -> x > 1).len()
        end
        ",
    );
    let ExprKind::MethodCall {
        receiver, method, ..
    } = &expr_of(&body[0]).kind
    else {
        panic!("expected a method call")
    };
    assert_name(&source, method, "len");
    let ExprKind::MethodCall {
        receiver, method, ..
    } = &receiver.kind
    else {
        panic!("expected an inner method call")
    };
    assert_name(&source, method, "filter");
    assert_eq!(text_at(&source, &receiver.span), "items");
}

#[test]
fn field_access_records_field_span() {
    let (source, body) = first_function_body(
        "
        fn x_of(point: Point) -> Int
          point.x
        end
        ",
    );
    let ExprKind::FieldAccess { field, .. } = &expr_of(&body[0]).kind else {
        panic!("expected a field access")
    };
    assert_name(&source, field, "x");
}

#[test]
fn function_reference_has_one_span_per_segment() {
    let (source, body) = first_function_body(
        "
        fn pick -> fn (Int, Int) -> Int
          &Math.add/2
        end
        ",
    );
    let ExprKind::NamedFunctionReference { path, arity, .. } = &expr_of(&body[0]).kind else {
        panic!("expected a function reference")
    };
    assert_path(&source, path, &["Math", "add"]);
    assert_eq!(*arity, 2);
}

#[test]
fn enum_and_constructor_patterns_record_spans() {
    let (source, body) = first_function_body(
        "
        fn describe(value: Option<Int>) -> Int
          match value
            Option.Some(n) -> n
            Some(n) -> n
            Color.Red -> 0
            Shape.Rect { width: w } -> w
            Point { x: px } -> px
          end
        end
        ",
    );
    let ExprKind::Match { arms, .. } = &expr_of(&body[0]).kind else {
        panic!("expected a match")
    };

    let Pattern::EnumTuple {
        type_path,
        variant,
        elements,
        ..
    } = &arms[0].pattern
    else {
        panic!("expected an enum tuple pattern")
    };
    assert_path(&source, type_path, &["Option"]);
    assert_name(&source, variant, "Some");
    let Pattern::Binding { name, .. } = &elements[0] else {
        panic!("expected a binding")
    };
    assert_name(&source, name, "n");

    let Pattern::Constructor { name, .. } = &arms[1].pattern else {
        panic!("expected a constructor pattern")
    };
    assert_name(&source, name, "Some");

    let Pattern::EnumUnit {
        type_path, variant, ..
    } = &arms[2].pattern
    else {
        panic!("expected an enum unit pattern")
    };
    assert_path(&source, type_path, &["Color"]);
    assert_name(&source, variant, "Red");

    let Pattern::EnumStruct {
        type_path,
        variant,
        fields,
        ..
    } = &arms[3].pattern
    else {
        panic!("expected an enum struct pattern")
    };
    assert_path(&source, type_path, &["Shape"]);
    assert_name(&source, variant, "Rect");
    assert_name(&source, &fields[0].name, "width");

    let Pattern::Struct {
        type_path, fields, ..
    } = &arms[4].pattern
    else {
        panic!("expected a struct pattern")
    };
    assert_path(&source, type_path, &["Point"]);
    assert_name(&source, &fields[0].name, "x");
}

#[test]
fn constructions_record_type_path_spans() {
    let (source, body) = first_function_body(
        "
        fn build -> Point
          p = Point { x: 1 }
          Process.ExitSignal.Normal
        end
        ",
    );
    let ExprKind::StructConstruction { type_path, fields } = &expr_of(&body[0]).kind else {
        panic!("expected a struct construction")
    };
    assert_path(&source, type_path, &["Point"]);
    assert_name(&source, &fields[0].name, "x");

    let ExprKind::EnumConstruction {
        type_path, variant, ..
    } = &expr_of(&body[1]).kind
    else {
        panic!("expected an enum construction")
    };
    assert_path(&source, type_path, &["Process", "ExitSignal"]);
    assert_name(&source, variant, "Normal");
}

#[test]
fn params_type_params_and_closure_params_record_name_spans() {
    let (source, ast) = file(
        "
        fn apply<T: Debug>(value: T, f: fn (T) -> T) -> T
          g = fn(item: T) -> T
            f(item)
          end
          g(value)
        end
        ",
    );
    let Item::Function(f) = &ast.items[0] else {
        panic!("expected a function")
    };
    assert_name(&source, &f.type_params[0].name, "T");
    let Param::Regular { name, .. } = &f.params[0] else {
        panic!("expected a regular param")
    };
    assert_name(&source, name, "value");

    let body = f.body.as_ref().expect("body");
    let ExprKind::Closure { params, .. } = &expr_of(&body[0]).kind else {
        panic!("expected a closure")
    };
    let ClosureParam::Name { name, .. } = &params[0] else {
        panic!("expected a named closure param")
    };
    assert_name(&source, name, "item");
}

#[test]
fn assignment_targets_record_segment_spans() {
    let (source, body) = first_function_body(
        "
        fn move(point: Point) -> Point
          point.x = 5
          total = 1
          point
        end
        ",
    );
    let Statement::Assignment { target, .. } = &body[0] else {
        panic!("expected an assignment")
    };
    assert_path(&source, &target.segments, &["point", "x"]);
    let Statement::Assignment { target, .. } = &body[1] else {
        panic!("expected an assignment")
    };
    assert_path(&source, &target.segments, &["total"]);
}

#[test]
fn alias_records_path_and_local_name_spans() {
    let (source, ast) = file(
        "
        alias JSON.Decoder as Reader
        ",
    );
    let Item::Alias(alias) = &ast.items[0] else {
        panic!("expected an alias")
    };
    assert_path(&source, &alias.path, &["JSON", "Decoder"]);
    assert_name(&source, &alias.local_name, "Reader");
}

#[test]
fn alias_without_local_name_reuses_the_last_segment_span() {
    let (source, ast) = file(
        "
        alias JSON.Decoder
        ",
    );
    let Item::Alias(alias) = &ast.items[0] else {
        panic!("expected an alias")
    };
    assert_name(&source, &alias.local_name, "Decoder");
    assert_eq!(alias.local_name.span, alias.path[1].span);
}
