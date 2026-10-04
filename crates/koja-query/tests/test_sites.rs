//! Test locations over a typechecked program, asserted as 1-indexed
//! start lines into the dedented source.

use std::path::PathBuf;

use koja_ast::util::dedent;
use koja_parser::{ParseMode, SourceFile, SourceTable, parse_program};
use koja_query::Analysis;
use koja_query::test_sites::tests_in_file;
use koja_typecheck::{CheckedProgram, check_program};

const PACKAGE: &str = "TestApp";
const MAIN: &str = "main.koja";

/// A typechecked program with the sources its spans index into.
struct Checked {
    program: CheckedProgram,
    sources: SourceTable,
}

impl Checked {
    fn analysis(&self) -> Analysis<'_> {
        Analysis::from_checked(&self.program, &self.sources)
    }
}

fn check(source: &str) -> Checked {
    let mut sources = koja_stdlib::autoimport_sources();
    sources.extend(koja_stdlib::qualified_sources());
    sources.push(SourceFile {
        package: PACKAGE.to_string(),
        path: PathBuf::from(MAIN),
        source: dedent(source),
    });
    let parsed = parse_program(sources, ParseMode::File);
    let sources = parsed.source_table();
    let program = check_program(parsed).unwrap_or_else(|failure| {
        let messages: Vec<&str> = failure
            .diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect();
        panic!("typecheck failed: {messages:?}")
    });
    Checked { program, sources }
}

fn test_lines(source: &str) -> Vec<u32> {
    let checked = check(source);
    let analysis = checked.analysis();
    let main = analysis.file_id(&PathBuf::from(MAIN)).expect("main file");
    tests_in_file(&analysis, main)
        .into_iter()
        .map(|span| span.start.line)
        .collect()
}

#[test]
fn finds_top_level_and_member_tests_in_source_order() {
    let lines = test_lines(
        "
        struct Stack
          items: List<Int>

          test \"member block\"
            assert Stack{items: List.new()}.items.length() == 0
          end

          fn helper(self) -> Int
            1
          end
        end

        test \"top-level block\"
          assert 1 == 1
        end

        extend Int
          test \"extend block\"
            assert 2 == 2
          end
        end
        ",
    );
    assert_eq!(lines, [4, 13, 18]);
}

#[test]
fn plain_functions_are_not_tests() {
    let lines = test_lines(
        "
        fn helper -> Int
          1
        end
        ",
    );
    assert!(lines.is_empty());
}

#[test]
fn legacy_annotated_functions_count_at_their_fn_line() {
    let lines = test_lines(
        "
        struct StackTest
          @test \"push then pop\"
          fn test_push_pop ! String
            ()
          end
        end
        ",
    );
    assert_eq!(lines, [3]);
}
