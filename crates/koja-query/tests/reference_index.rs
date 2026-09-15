//! The reference index over a typechecked program. Positions are
//! 1-indexed `(line, col)` into the dedented source.

use std::path::PathBuf;

use koja_ast::span::FileId;
use koja_ast::util::dedent;
use koja_parser::{ParseMode, SourceFile, parse_program};
use koja_query::{Analysis, Occurrence, ReferenceIndex, Role, SymbolKey};
use koja_typecheck::{CheckFailure, CheckedProgram, check_program};

const PACKAGE: &str = "TestApp";
const MAIN: &str = "main.koja";

fn sources(files: &[(&str, &str)]) -> Vec<SourceFile> {
    let mut sources = koja_stdlib::autoimport_sources();
    sources.extend(koja_stdlib::qualified_sources());
    for (path, source) in files {
        sources.push(SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from(path),
            source: dedent(source),
        });
    }
    sources
}

fn check(source: &str) -> CheckedProgram {
    check_files(&[(MAIN, source)])
}

fn check_files(files: &[(&str, &str)]) -> CheckedProgram {
    let parsed = parse_program(sources(files), ParseMode::File);
    check_program(parsed).unwrap_or_else(|failure| {
        let messages: Vec<&str> = failure
            .diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect();
        panic!("typecheck failed: {messages:?}")
    })
}

fn fail(source: &str) -> CheckFailure {
    let parsed = parse_program(sources(&[(MAIN, source)]), ParseMode::File);
    check_program(parsed).expect_err("typecheck should fail")
}

fn project_index(analysis: &Analysis<'_>) -> ReferenceIndex {
    ReferenceIndex::build_filtered(analysis, |file| file.package == PACKAGE)
}

fn file_id(analysis: &Analysis<'_>, path: &str) -> FileId {
    analysis
        .file_id(&PathBuf::from(path))
        .unwrap_or_else(|| panic!("no file {path}"))
}

fn at(index: &ReferenceIndex, file: FileId, line: u32, col: u32) -> &Occurrence {
    index
        .occurrence_at(file, line, col)
        .unwrap_or_else(|| panic!("no occurrence at {line}:{col}"))
}

fn positions(index: &ReferenceIndex, key: SymbolKey) -> Vec<(u32, u32, Role)> {
    let mut out: Vec<(u32, u32, Role)> = index
        .occurrences(key)
        .map(|o| (o.span.start.line, o.span.start.column, o.role))
        .collect();
    out.sort_by_key(|(line, col, _)| (*line, *col));
    out
}

#[test]
fn locals_in_two_functions_do_not_share_a_key() {
    let checked = check(
        r#"
        fn first(a: Int) -> Int
          total = a
          total
        end

        fn second(b: Int) -> Int
          total = b
          total
        end
        "#,
    );
    let analysis = Analysis::from_checked(&checked);
    let index = project_index(&analysis);
    let main = file_id(&analysis, MAIN);

    let first_total = at(&index, main, 2, 3);
    let second_total = at(&index, main, 7, 3);
    assert!(matches!(first_total.key, SymbolKey::Local { .. }));
    assert_ne!(first_total.key, second_total.key);
    assert_eq!(
        positions(&index, first_total.key),
        vec![(2, 3, Role::Declaration), (3, 3, Role::Read)]
    );
    assert_eq!(first_total.role, Role::Declaration);
    assert!(
        first_total.ty.is_some(),
        "declaration carries the initializer type"
    );
    assert_eq!(
        index
            .declaration(second_total.key)
            .map(|o| o.span.start.line),
        Some(7)
    );
}

#[test]
fn global_used_from_a_sibling_file() {
    let checked = check_files(&[
        (
            "helper.koja",
            r#"
            fn helper() -> Int
              1
            end
            "#,
        ),
        (
            MAIN,
            r#"
            fn run() -> Int
              helper()
            end
            "#,
        ),
    ]);
    let analysis = Analysis::from_checked(&checked);
    let index = project_index(&analysis);
    let main = file_id(&analysis, MAIN);
    let helper = file_id(&analysis, "helper.koja");

    let call = at(&index, main, 2, 3);
    let SymbolKey::Global(id) = call.key else {
        panic!("expected a global key, got {:?}", call.key);
    };
    assert_eq!(
        analysis.registry.get(id).unwrap().identifier.last(),
        "helper"
    );
    let declaration = index.declaration(call.key).expect("declaration indexed");
    assert_eq!(declaration.span.file, helper);
    assert_eq!(
        (declaration.span.start.line, declaration.span.start.column),
        (1, 4)
    );
    assert_eq!(index.occurrences(call.key).count(), 2);
}

#[test]
fn type_parameter_declaration_and_reads() {
    let checked = check(
        r#"
        fn identity<T>(x: T) -> T
          x
        end
        "#,
    );
    let analysis = Analysis::from_checked(&checked);
    let index = project_index(&analysis);
    let main = file_id(&analysis, MAIN);

    let declaration = at(&index, main, 1, 13);
    assert!(matches!(declaration.key, SymbolKey::TypeParam { .. }));
    assert_eq!(
        positions(&index, declaration.key),
        vec![
            (1, 13, Role::Declaration),
            (1, 19, Role::Read),
            (1, 25, Role::Read)
        ]
    );
}

#[test]
fn method_call_split_across_a_newline() {
    let checked = check(
        r#"
        struct Counter
          n: Int

          fn bump(self) -> Counter
            Counter { n: self.n + 1 }
          end
        end

        fn run() -> Counter
          Counter { n: 0 }
            .bump()
        end
        "#,
    );
    let analysis = Analysis::from_checked(&checked);
    let index = project_index(&analysis);
    let main = file_id(&analysis, MAIN);

    let call = at(&index, main, 11, 6);
    assert_eq!(call.name, "bump");
    assert_eq!(
        positions(&index, call.key),
        vec![(4, 6, Role::Declaration), (11, 6, Role::Read)]
    );

    let counter = at(&index, main, 1, 8);
    let counter_positions = positions(&index, counter.key);
    assert!(counter_positions.contains(&(1, 8, Role::Declaration)));
    assert!(
        counter_positions.contains(&(4, 20, Role::Read)),
        "return type"
    );
    assert!(
        counter_positions.contains(&(5, 5, Role::Read)),
        "construction in body"
    );
    assert!(
        counter_positions.contains(&(10, 3, Role::Read)),
        "construction in run"
    );
}

#[test]
fn qualified_type_in_a_field_annotation() {
    let checked = check(
        r#"
        struct Bag
          items: List<Int>
        end
        "#,
    );
    let analysis = Analysis::from_checked(&checked);
    let index = project_index(&analysis);
    let main = file_id(&analysis, MAIN);

    let list = at(&index, main, 2, 10);
    assert_eq!(list.name, "List");
    let int = at(&index, main, 2, 15);
    assert_eq!(int.name, "Int");
    assert_ne!(list.key, int.key);
    let SymbolKey::Global(id) = list.key else {
        panic!("type path stamps a global");
    };
    assert_eq!(analysis.registry.get(id).unwrap().identifier.last(), "List");
}

#[test]
fn enum_pattern_paths_reference_the_enum() {
    let checked = check(
        r#"
        enum Color
          Red
          Blue
        end

        fn name(c: Color) -> String
          match c
            Color.Red -> "red"
            Color.Blue -> "blue"
          end
        end
        "#,
    );
    let analysis = Analysis::from_checked(&checked);
    let index = project_index(&analysis);
    let main = file_id(&analysis, MAIN);

    let color = at(&index, main, 1, 6);
    assert_eq!(
        positions(&index, color.key),
        vec![
            (1, 6, Role::Declaration),
            (6, 12, Role::Read),
            (8, 5, Role::Read),
            (9, 5, Role::Read),
        ]
    );
}

#[test]
fn assignment_after_declaration_is_a_write() {
    let checked = check(
        r#"
        fn count() -> Int
          n = 0
          n = n + 1
          n
        end
        "#,
    );
    let analysis = Analysis::from_checked(&checked);
    let index = project_index(&analysis);
    let main = file_id(&analysis, MAIN);

    let n = at(&index, main, 2, 3);
    assert_eq!(
        positions(&index, n.key),
        vec![
            (2, 3, Role::Declaration),
            (3, 3, Role::Write),
            (3, 7, Role::Read),
            (4, 3, Role::Read),
        ]
    );
}

#[test]
fn stdlib_reference_resolves_to_its_declaration_span() {
    let checked = check(
        r#"
        fn id(x: Int) -> Int
          x
        end
        "#,
    );
    let analysis = Analysis::from_checked(&checked);
    let index = project_index(&analysis);
    let main = file_id(&analysis, MAIN);

    let int = at(&index, main, 1, 10);
    assert!(
        index.declaration(int.key).is_none(),
        "stdlib files are not indexed"
    );
    let span = index
        .declaration_span(int.key, analysis.registry)
        .expect("registry supplies the declaration span");
    assert_ne!(span.file, main);
    assert!(!span.synthetic);
}

#[test]
fn desugared_calls_leave_no_occurrence() {
    let checked = check(
        r#"
        struct Point
          x: Int
        end

        fn same(a: Point, b: Point) -> Bool
          a == b
        end

        fn label(n: Int) -> String
          "n is #{n}"
        end

        fn first(items: List<Int>) -> Int
          match items.first()
            Some(x) -> x
            None -> 0
          end
        end
        "#,
    );
    let analysis = Analysis::from_checked(&checked);
    let index = project_index(&analysis);
    let main = file_id(&analysis, MAIN);

    let names: Vec<&str> = index
        .iter()
        .filter(|o| o.span.file == main)
        .map(|o| o.name.as_str())
        .collect();
    assert!(!names.contains(&"eq"), "{names:?}");
    assert!(!names.contains(&"format"), "{names:?}");
    assert!(!names.contains(&"Option"), "{names:?}");
    // `a` in `a == b` is still a read of the parameter.
    let a = at(&index, main, 6, 3);
    assert_eq!(a.name, "a");
    assert_eq!(a.role, Role::Read);
    // `x` bound by the `Some(x)` shorthand is a real local.
    let x = at(&index, main, 15, 10);
    assert_eq!(
        positions(&index, x.key),
        vec![(15, 10, Role::Declaration), (15, 16, Role::Read)]
    );
}

#[test]
fn a_program_with_a_type_error_still_answers() {
    let failure = fail(
        r#"
        fn broken() -> Int
          "not an int"
        end

        fn fine(x: Int) -> Int
          x
        end
        "#,
    );
    let analysis = Analysis::from_failure(&failure).expect("registry survives a type error");
    assert!(analysis.has_errors);
    let index = project_index(&analysis);
    let main = file_id(&analysis, MAIN);

    let x = at(&index, main, 6, 3);
    assert_eq!(
        positions(&index, x.key),
        vec![(5, 9, Role::Declaration), (6, 3, Role::Read)]
    );
    let fine = at(&index, main, 5, 4);
    assert!(matches!(fine.key, SymbolKey::Global(_)));
}

#[test]
fn static_receiver_records_the_type_name_only() {
    let checked = check(
        r#"
        struct Point
          x: Int

          fn origin() -> Point
            Point { x: 0 }
          end
        end

        fn run() -> Point
          Point.origin()
        end
        "#,
    );
    let analysis = Analysis::from_checked(&checked);
    let index = project_index(&analysis);
    let main = file_id(&analysis, MAIN);

    let point = at(&index, main, 10, 3);
    assert_eq!(point.name, "Point");
    assert_eq!((point.span.start.column, point.span.end.column), (3, 8));
    let origin = at(&index, main, 10, 9);
    assert_eq!(origin.name, "origin");
    assert_eq!(
        positions(&index, origin.key),
        vec![(4, 6, Role::Declaration), (10, 9, Role::Read)]
    );
}
