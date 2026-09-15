//! Inlay hints over a typechecked program. Hints are asserted as
//! `(line, col, label)` with 1-indexed positions into the dedented
//! source.

use std::path::PathBuf;

use koja_ast::span::{FileId, Position, Span};
use koja_ast::util::dedent;
use koja_parser::{ParseMode, SourceFile, parse_program};
use koja_query::inlay::{HintKind, hints};
use koja_query::{Analysis, ReferenceIndex};
use koja_typecheck::{CheckedProgram, check_program};

const PACKAGE: &str = "TestApp";
const MAIN: &str = "main.koja";

fn check(source: &str) -> CheckedProgram {
    let mut sources = koja_stdlib::autoimport_sources();
    sources.extend(koja_stdlib::qualified_sources());
    sources.push(SourceFile {
        package: PACKAGE.to_string(),
        path: PathBuf::from(MAIN),
        source: dedent(source),
    });
    let parsed = parse_program(sources, ParseMode::File);
    check_program(parsed).unwrap_or_else(|failure| {
        let messages: Vec<&str> = failure
            .diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect();
        panic!("typecheck failed: {messages:?}")
    })
}

fn span(file: FileId, start: (u32, u32), end: (u32, u32)) -> Span {
    let at = |(line, column): (u32, u32)| Position {
        offset: 0,
        line,
        column,
    };
    Span {
        start: at(start),
        end: at(end),
        file,
        synthetic: false,
    }
}

/// Every hint in the main file, over the whole file.
fn all_hints(source: &str) -> Vec<(u32, u32, String, HintKind)> {
    hints_in(source, (1, 1), (u32::MAX, u32::MAX))
}

fn hints_in(source: &str, start: (u32, u32), end: (u32, u32)) -> Vec<(u32, u32, String, HintKind)> {
    let checked = check(source);
    let analysis = Analysis::from_checked(&checked);
    let index = ReferenceIndex::build_filtered(&analysis, |file| file.package == PACKAGE);
    let main = analysis.file_id(&PathBuf::from(MAIN)).expect("main file");
    hints(&analysis, &index, main, span(main, start, end))
        .into_iter()
        .map(|hint| {
            (
                hint.position.line,
                hint.position.column,
                hint.label,
                hint.kind,
            )
        })
        .collect()
}

fn ty(line: u32, col: u32, label: &str) -> (u32, u32, String, HintKind) {
    (line, col, label.to_string(), HintKind::Type)
}

fn param(line: u32, col: u32, label: &str) -> (u32, u32, String, HintKind) {
    (line, col, label.to_string(), HintKind::Parameter)
}

#[test]
fn declaration_gets_a_type_and_rebind_does_not() {
    let found = all_hints(
        "
        fn main
          x = 1
          x = 2
          x
        end
        ",
    );
    assert_eq!(found, vec![ty(2, 4, ": Int")]);
}

#[test]
fn annotated_declaration_gets_nothing() {
    let found = all_hints(
        "
        fn main
          x: Int = 1
          x
        end
        ",
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn untyped_closure_params_get_types() {
    let found = all_hints(
        "
        fn apply(f: fn (Int, Int) -> Int) -> Int
          f(1, 2)
        end

        fn typed(f: fn (Int) -> Int) -> Int
          f(1)
        end

        fn main
          a = apply(fn (a, b) -> Int
            a + b
          end)
          b = typed(fn (a: Int) -> Int
            a
          end)
          a + b
        end
        ",
    );
    assert_eq!(
        found,
        vec![
            ty(10, 4, ": Int"),
            param(10, 13, "f:"),
            ty(10, 18, ": Int"),
            ty(10, 21, ": Int"),
            ty(13, 4, ": Int"),
            param(13, 13, "f:"),
        ]
    );
}

#[test]
fn short_closure_params_get_types() {
    let found = all_hints(
        "
        fn apply(f: fn (Int) -> Int) -> Int
          f(1)
        end

        fn main
          apply(x -> x * 2)
        end
        ",
    );
    assert_eq!(found, vec![param(6, 9, "f:"), ty(6, 10, ": Int")]);
}

#[test]
fn destructure_bindings_get_types() {
    let found = all_hints(
        r#"
        fn pair -> (Int, String)
          (1, "one")
        end

        fn main
          (n, s) = pair()
          s
        end
        "#,
    );
    assert_eq!(found, vec![ty(6, 5, ": Int"), ty(6, 8, ": String")]);
}

#[test]
fn positional_args_get_parameter_names() {
    let found = all_hints(
        "
        fn add(a: Int, b: Int) -> Int
          a + b
        end

        fn main
          add(1, 2)
        end
        ",
    );
    assert_eq!(found, vec![param(6, 7, "a:"), param(6, 10, "b:")]);
}

#[test]
fn args_named_like_their_parameter_get_nothing() {
    let found = all_hints(
        "
        fn add(a: Int, b: Int) -> Int
          a + b
        end

        fn main
          a = 1
          b = 2
          add(a, b)
        end
        ",
    );
    assert_eq!(found, vec![ty(6, 4, ": Int"), ty(7, 4, ": Int")]);
}

#[test]
fn instance_call_skips_self_and_static_call_does_not() {
    let found = all_hints(
        "
        struct Counter
          n: Int

          fn bump(self, by: Int) -> Counter
            Counter { n: self.n + by }
          end

          fn make(start: Int) -> Counter
            Counter { n: start }
          end
        end

        fn main
          c = Counter.make(1)
          c.bump(2)
        end
        ",
    );
    assert_eq!(
        found,
        vec![
            ty(14, 4, ": Counter"),
            param(14, 20, "start:"),
            param(15, 10, "by:"),
        ]
    );
}

#[test]
fn desugared_equality_produces_nothing() {
    let found = all_hints(
        "
        fn main -> Bool
          a = 1
          b = 2
          a == b
        end
        ",
    );
    assert_eq!(found, vec![ty(2, 4, ": Int"), ty(3, 4, ": Int")]);
}

#[test]
fn range_filters_by_position() {
    let source = "
        fn main
          x = 1
          y = 2
          z = 3
          x + y + z
        end
        ";
    assert_eq!(hints_in(source, (3, 1), (3, 99)), vec![ty(3, 4, ": Int")]);
    assert_eq!(
        hints_in(source, (2, 1), (3, 99)),
        vec![ty(2, 4, ": Int"), ty(3, 4, ": Int")]
    );
}
