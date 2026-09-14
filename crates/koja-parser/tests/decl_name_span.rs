//! Coverage for the `Name` on each declaration kind: its `span`
//! covers the name token alone, not the keyword or the body.
//!
//! Pins:
//! - every declaration kind records a name span that slices back to
//!   the name text
//! - a dotted `struct A.B` path records one span per segment
//! - the name span sits on the declaration's first line and inside
//!   the declaration span

use koja_ast::ast::{Item, Name};
use koja_ast::span::Span;
use koja_ast::util::dedent;
use koja_parser::{ParseMode, parse};

/// The source text under `span`.
fn text_at<'a>(source: &'a str, span: &Span) -> &'a str {
    &source[span.start.offset as usize..span.end.offset as usize]
}

fn assert_name(source: &str, name: &Name, expected: &str, decl_span: &Span) {
    assert_eq!(name, expected);
    assert_eq!(text_at(source, &name.span), expected);
    assert_eq!(name.span.start.line, decl_span.start.line);
    assert!(name.span.start.offset >= decl_span.start.offset);
    assert!(name.span.end.offset <= decl_span.end.offset);
}

fn items(source: &str) -> (String, Vec<Item>) {
    let source = dedent(source);
    let result = parse(&source, ParseMode::File);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    (source, result.ast.items)
}

#[test]
fn function_name_span() {
    let (source, items) = items(
        "
        priv fn greet(name: String) -> String
          name
        end
        ",
    );
    let Item::Function(f) = &items[0] else {
        panic!("expected a function")
    };
    assert_name(&source, &f.name, "greet", &f.span);
}

#[test]
fn struct_and_inline_method_name_spans() {
    let (source, items) = items(
        "
        struct Point<T>
          x: T

          fn origin -> Point<T>
            Point{x: 0}
          end
        end
        ",
    );
    let Item::Struct(s) = &items[0] else {
        panic!("expected a struct")
    };
    assert_name(&source, s.name(), "Point", &s.span);
    assert_eq!(text_at(&source, &s.functions[0].name.span), "origin");
}

#[test]
fn nested_struct_path_has_one_span_per_segment() {
    let (source, items) = items(
        "
        struct Process
        end

        struct Process.ExitSignal
          code: Int
        end
        ",
    );
    let Item::Struct(s) = &items[1] else {
        panic!("expected a struct")
    };
    assert_eq!(s.path.len(), 2);
    assert_eq!(text_at(&source, &s.path[0].span), "Process");
    assert_eq!(text_at(&source, &s.path[1].span), "ExitSignal");
    assert_name(&source, s.name(), "ExitSignal", &s.span);
}

#[test]
fn enum_name_span() {
    let (source, items) = items(
        "
        enum Color
          Red
          Green
        end
        ",
    );
    let Item::Enum(e) = &items[0] else {
        panic!("expected an enum")
    };
    assert_name(&source, e.name(), "Color", &e.span);
}

#[test]
fn builtin_name_span() {
    let (source, items) = items(
        "
        builtin List<T>
          @intrinsic
          fn len(self) -> Int
        end
        ",
    );
    let Item::Builtin(b) = &items[0] else {
        panic!("expected a builtin")
    };
    assert_name(&source, b.name(), "List", &b.span);
}

#[test]
fn protocol_and_method_name_spans() {
    let (source, items) = items(
        "
        protocol Shape
          fn area(self) -> Float
        end
        ",
    );
    let Item::Protocol(p) = &items[0] else {
        panic!("expected a protocol")
    };
    assert_name(&source, &p.name, "Shape", &p.span);
    assert_name(&source, &p.methods[0].name, "area", &p.methods[0].span);
}

#[test]
fn constant_name_span() {
    let (source, items) = items(
        "
        const MAX_RETRIES: Int = 3
        ",
    );
    let Item::Constant(c) = &items[0] else {
        panic!("expected a constant")
    };
    assert_name(&source, &c.name, "MAX_RETRIES", &c.span);
}

#[test]
fn type_alias_name_span() {
    let (source, items) = items(
        "
        type Handler = fn (Int) -> Int
        ",
    );
    let Item::TypeAlias(t) = &items[0] else {
        panic!("expected a type alias")
    };
    assert_name(&source, &t.name, "Handler", &t.span);
}
