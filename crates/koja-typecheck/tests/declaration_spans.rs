//! Declaration diagnostics point at the name token, not the whole
//! declaration, and the registry keeps that name span next to the
//! declaration span.
//!
//! Pins:
//! - `already defined` lands on the duplicate's name and its related
//!   location is the earlier name
//! - nested-type owner errors land on the leaf name
//! - signature-leak errors land on the leaking declaration's name
//! - `RegistryEntry::name_span` slices back to the name text while
//!   `span` still covers the declaration

use koja_ast::ast::Diagnostic;
use koja_ast::identifier::Identifier;
use koja_ast::span::Span;
use koja_ast::util::dedent;

mod common;

use common::{PACKAGE, typecheck_file, typecheck_file_fail};

/// The source text under `span`.
fn text_at<'a>(source: &'a str, span: &Span) -> &'a str {
    &source[span.start.offset as usize..span.end.offset as usize]
}

fn diagnostic_containing<'a>(diagnostics: &'a [Diagnostic], needle: &str) -> &'a Diagnostic {
    diagnostics
        .iter()
        .find(|d| d.message.contains(needle))
        .unwrap_or_else(|| panic!("no diagnostic contains `{needle}`: {diagnostics:#?}"))
}

#[test]
fn duplicate_function_points_at_the_second_name() {
    let source = dedent(
        "
        fn greet
          1
        end

        fn greet
          2
        end
        ",
    );
    let failure = typecheck_file_fail(&source);
    let diagnostic = diagnostic_containing(&failure.diagnostics, "already defined");
    assert_eq!(text_at(&source, &diagnostic.span), "greet");
    assert_eq!(diagnostic.span.start.line, 5);
    let related = diagnostic.related.as_ref().expect("related location");
    assert_eq!(related.message, "previous function definition");
    assert_eq!(text_at(&source, &related.span), "greet");
    assert_eq!(related.span.start.line, 1);
}

#[test]
fn duplicate_struct_points_at_the_second_name() {
    let source = dedent(
        "
        struct Point
          x: Int
        end

        priv struct Point
          y: Int
        end
        ",
    );
    let failure = typecheck_file_fail(&source);
    let diagnostic = diagnostic_containing(&failure.diagnostics, "already defined");
    assert_eq!(text_at(&source, &diagnostic.span), "Point");
    assert_eq!(diagnostic.span.start.line, 5);
}

/// The derived `Debug` and `Equality` impls are synthesized for the
/// first declaration only, so they add no errors of their own.
#[test]
fn duplicate_struct_reports_once() {
    let source = dedent(
        "
        struct Point
          x: Int
        end

        struct Point
          y: Int
        end
        ",
    );
    let failure = typecheck_file_fail(&source);
    let messages: Vec<&str> = failure
        .diagnostics
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(
        messages,
        vec![format!("`{PACKAGE}.Point` is already defined")]
    );
}

#[test]
fn nested_type_under_unknown_owner_points_at_the_leaf_name() {
    let source = dedent(
        "
        struct Missing.Inner
          x: Int
        end
        ",
    );
    let failure = typecheck_file_fail(&source);
    let diagnostic = diagnostic_containing(&failure.diagnostics, "must be declared under");
    assert_eq!(text_at(&source, &diagnostic.span), "Inner");
}

#[test]
fn signature_leak_points_at_the_public_name() {
    let source = dedent(
        "
        priv struct Hidden
          x: Int
        end

        fn expose(h: Hidden) -> Int
          h.x
        end
        ",
    );
    let failure = typecheck_file_fail(&source);
    let diagnostic = diagnostic_containing(&failure.diagnostics, "exposes private");
    assert_eq!(text_at(&source, &diagnostic.span), "expose");
}

#[test]
fn registry_entry_keeps_both_spans() {
    let source = dedent(
        "
        struct Point<T>
          x: T
        end

        fn origin -> Point<Int>
          Point{x: 0}
        end
        ",
    );
    let checked = typecheck_file(&source);

    let (_, point) = checked
        .registry
        .lookup(&Identifier::single(PACKAGE, "Point"))
        .expect("Point is registered");
    assert_eq!(text_at(&source, &point.name_span), "Point");
    assert!(text_at(&source, &point.span).starts_with("struct Point<T>"));

    let (_, origin) = checked
        .registry
        .lookup_function(&Identifier::single(PACKAGE, "origin"), 0)
        .expect("origin is registered");
    assert_eq!(text_at(&source, &origin.name_span), "origin");
    assert!(text_at(&source, &origin.span).starts_with("fn origin"));
}
