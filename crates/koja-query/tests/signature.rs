//! Hover signatures built from the registry and laid out by
//! `koja-fmt`.

use std::path::PathBuf;

use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_parser::{ParseMode, SourceFile, parse_program};
use koja_query::Analysis;
use koja_query::signature::function_signature;
use koja_typecheck::{CheckedProgram, check_program};

const PACKAGE: &str = "TestApp";

fn check(source: &str) -> CheckedProgram {
    let mut sources = koja_stdlib::autoimport_sources();
    sources.extend(koja_stdlib::qualified_sources());
    sources.push(SourceFile {
        package: PACKAGE.to_string(),
        path: PathBuf::from("main.koja"),
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

/// The formatted signature of `path` at `arity` in the test package.
fn signature(checked: &CheckedProgram, path: &[&str], arity: usize) -> String {
    let identifier = Identifier::new(PACKAGE, path.iter().map(|s| s.to_string()).collect());
    let (id, _) = checked
        .registry
        .lookup_function(&identifier, arity)
        .unwrap_or_else(|| panic!("no function {identifier:?}/{arity}"));
    let analysis = Analysis::from_checked(checked);
    function_signature(&analysis, id).expect("signature is lifted")
}

#[test]
fn short_function_stays_on_one_line() {
    let checked = check(
        "
        fn add(a: Int, b: Int) -> Int
          a + b
        end
        ",
    );
    assert_eq!(
        signature(&checked, &["add"], 2),
        "fn add(a: Int, b: Int) -> Int"
    );
}

#[test]
fn unit_return_is_left_out() {
    let checked = check(
        "
        fn noop(a: Int)
          ()
        end
        ",
    );
    assert_eq!(signature(&checked, &["noop"], 1), "fn noop(a: Int)");
}

#[test]
fn long_method_breaks_one_parameter_per_line() {
    let checked = check(
        "
        struct Connection
          priv fn prepare_and_run(self, key: String, sql: String, oids: List<Int>, texts: List<Option<String>>, stale_close: Binary) -> (Connection, Result<Int, String>)
            (self, Result.Ok(1))
          end
        end
        ",
    );
    assert_eq!(
        signature(&checked, &["Connection", "prepare_and_run"], 6),
        dedent(
            "
            priv fn Connection.prepare_and_run(
              self,
              key: String,
              sql: String,
              oids: List<Int>,
              texts: List<Option<String>>,
              stale_close: Binary,
            ) -> (Connection, Result<Int, String>)"
        )
        .trim_start()
    );
}

#[test]
fn alias_shows_the_target_type() {
    let checked = check(
        "
        alias Crypto.SHA256 as Hasher

        fn digest(h: Hasher) -> Hasher
          h
        end
        ",
    );
    assert_eq!(
        signature(&checked, &["digest"], 1),
        "fn digest(h: SHA256) -> SHA256"
    );
}

#[test]
fn fallible_return_keeps_the_bang_spelling() {
    let checked = check(
        "
        fn parse(s: String) -> Int ! String
          1
        end

        fn wrapped(s: String) -> Result<Int, String>
          Result.Ok(1)
        end
        ",
    );
    assert_eq!(
        signature(&checked, &["parse"], 1),
        "fn parse(s: String) -> Int ! String"
    );
    assert_eq!(
        signature(&checked, &["wrapped"], 1),
        "fn wrapped(s: String) -> Result<Int, String>"
    );
}

#[test]
fn generic_function_keeps_its_bounds() {
    let checked = check(
        "
        fn show<T: Debug>(value: T) -> String
          value.format()
        end
        ",
    );
    assert_eq!(
        signature(&checked, &["show"], 1),
        "fn show<T: Debug>(value: T) -> String"
    );
}

#[test]
fn derived_equals_renders() {
    // Every struct derives `Equality`. The synthesized method has no
    // source, so the signature comes from the registry alone.
    let checked = check(
        "
        struct Point
          x: Int
        end
        ",
    );
    assert_eq!(
        signature(&checked, &["Point", "equals?"], 2),
        "fn Point.equals?(self, other: Point) -> Bool"
    );
}
