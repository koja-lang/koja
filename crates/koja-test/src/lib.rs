//! Test discovery and harness synthesis for `koja test`.
//!
//! The driver feeds a parsed project (sources + test fixtures) into
//! [`discover_tests`] to enumerate every `test "..."` block and
//! `@test`-annotated function belonging to the current project.
//! [`generate_harness`] then
//! produces a Koja source string for a synthetic
//! [`HARNESS_ENTRY`] type implementing `Process<(), (), ()>` whose
//! `run` invokes each test, tracks pass/fail counts, and stops with
//! `StopReason.Shutdown` (exit 1) when anything fails. The driver
//! splices that harness into the parsed program and lowers with
//! [`HARNESS_ENTRY`] as the project's Process entry.
//!
//! Kept backend-agnostic on purpose: this crate only depends on
//! the AST + parser surface, so any backend can share the same
//! harness shape.

use std::path::Path;

use koja_ast::ast::{AnnotationValue, Function, Item, TestDecl, TypeExpr, synthesized_test_name};
use koja_parser::ParsedProgram;

/// Name of the synthesized test-harness entry type. Reserved for
/// the test runner. The driver passes this as the project's
/// Process entry when lowering test builds, so it must match the
/// struct name emitted by [`generate_harness`].
pub const HARNESS_ENTRY: &str = "KojaTestHarness";

/// Output knobs for the synthesized harness.
///
/// `trace` swaps the compact dots-and-summary output for one group
/// header per struct and one timed line per test (modeled on
/// `mix test --trace`), and `color` gates the ANSI escapes so
/// `--no-color` / `NO_COLOR` reach the generated source.
#[derive(Clone, Copy, Debug, Default)]
pub struct TestOptions {
    pub color: bool,
    pub trace: bool,
}

/// A discovered test, called as `Outer.Inner.fn_name()` or bare
/// `fn_name()` from the generated harness. `file` and `line` record
/// the source location (`file` is rendered relative to the project
/// root) for navigable trace and failure output.
#[derive(Clone, Debug)]
pub struct TestCase {
    pub description: String,
    pub file: String,
    pub fn_name: String,
    pub line: u32,
    /// `None` for a top-level `test` block.
    pub owner: Option<Owner>,
}

/// The type a member test became a method on.
#[derive(Clone, Debug)]
pub struct Owner {
    /// The trace group header. The type path for a struct, enum, or
    /// builtin body, `Type: Protocol` for an `impl` block, and the
    /// target type for an `extend` block.
    pub group: String,
    /// The qualified type path the harness calls through,
    /// `["Outer", "Inner"]` for a nested type.
    pub path: Vec<String>,
}

impl TestCase {
    /// The qualified call the harness emits.
    fn call_path(&self) -> String {
        match &self.owner {
            Some(owner) => format!("{}.{}", owner.path.join("."), self.fn_name),
            None => self.fn_name.clone(),
        }
    }

    /// The trace group header. Member tests group under their owner,
    /// top-level tests under their file.
    fn group(&self) -> String {
        match &self.owner {
            Some(owner) => owner.group.clone(),
            None => self.file.clone(),
        }
    }
}

/// Walks the parsed program and collects top-level `test` blocks,
/// member `test` blocks in struct, enum, impl, extend, and builtin
/// bodies (through nested types), and `@test`-annotated struct
/// functions. Only scans files belonging to the current project
/// (matched by the per-file `package` field), so deps' fixtures don't
/// sneak into the harness. `root` relativizes each test's source path
/// for clean, navigable `path:line` output.
pub fn discover_tests(parsed: &ParsedProgram, project_name: &str, root: &Path) -> Vec<TestCase> {
    let mut tests = Vec::new();

    for file in parsed.iter() {
        if file.package != project_name {
            continue;
        }

        let display_path = file
            .path
            .strip_prefix(root)
            .unwrap_or(&file.path)
            .to_string_lossy()
            .into_owned();
        let mut collector = Collector {
            file: &display_path,
            path: file.ast.path.as_deref(),
            tests: &mut tests,
        };

        for item in &file.ast.items {
            match item {
                Item::Test(t) => collector.push(t.description.clone(), None, t.span.start.line),
                _ => collector.nested(item, &[]),
            }
        }
    }

    tests
}

struct Collector<'a> {
    file: &'a str,
    path: Option<&'a Path>,
    tests: &'a mut Vec<TestCase>,
}

impl Collector<'_> {
    fn push(&mut self, description: String, owner: Option<&Owner>, line: u32) {
        let fn_name = synthesized_test_name(self.path, line);
        self.push_named(description, fn_name, owner, line);
    }

    fn push_named(
        &mut self,
        description: String,
        fn_name: String,
        owner: Option<&Owner>,
        line: u32,
    ) {
        self.tests.push(TestCase {
            description,
            file: self.file.to_string(),
            fn_name,
            line,
            owner: owner.cloned(),
        });
    }

    fn member_tests(&mut self, tests: &[TestDecl], owner: &Owner) {
        for test in tests {
            self.push(test.description.clone(), Some(owner), test.span.start.line);
        }
    }

    fn nested(&mut self, item: &Item, outer: &[String]) {
        match item {
            Item::Builtin(b) => {
                let owner = Owner::type_path(qualify(outer, &b.path));
                self.member_tests(&b.tests, &owner);
            }
            Item::Enum(e) => {
                let owner = Owner::type_path(qualify(outer, &e.path));
                self.member_tests(&e.tests, &owner);
                for nested in &e.nested {
                    self.nested(nested, &owner.path);
                }
            }
            Item::Extend(block) => {
                if let Some(path) = type_expr_path(&block.target) {
                    let owner = Owner::type_path(path);
                    self.member_tests(&block.tests, &owner);
                }
            }
            Item::Impl(block) => {
                if let Some(path) = type_expr_path(&block.target) {
                    let group = match type_expr_path(&block.trait_expr) {
                        Some(protocol) => format!("{}: {}", path.join("."), protocol.join(".")),
                        None => path.join("."),
                    };
                    let owner = Owner { group, path };
                    self.member_tests(&block.tests, &owner);
                }
            }
            Item::Struct(s) => {
                let owner = Owner::type_path(qualify(outer, &s.path));
                self.member_tests(&s.tests, &owner);
                for func in &s.functions {
                    if let Some(description) = annotated_test_description(func) {
                        self.push_named(
                            description,
                            func.name.clone(),
                            Some(&owner),
                            func.span.start.line,
                        );
                    }
                }
                for nested in &s.nested {
                    self.nested(nested, &owner.path);
                }
            }
            _ => {}
        }
    }
}

impl Owner {
    fn type_path(path: Vec<String>) -> Self {
        Owner {
            group: path.join("."),
            path,
        }
    }
}

fn qualify(outer: &[String], path: &[String]) -> Vec<String> {
    outer.iter().chain(path).cloned().collect()
}

/// The type path of an `impl` or `extend` target. Type arguments are
/// dropped because the harness calls the static method through the
/// bare type. Targets that are not named types have no path.
fn type_expr_path(target: &TypeExpr) -> Option<Vec<String>> {
    match target {
        TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. } => Some(path.clone()),
        _ => None,
    }
}

/// The description of an `@test` function, or `None` when the
/// function has no `@test` annotation. A bare `@test` falls back to
/// the function name.
fn annotated_test_description(func: &Function) -> Option<String> {
    let ann = func.annotations.iter().find(|a| a.name == "test")?;
    Some(match &ann.value {
        Some(AnnotationValue::String(s)) => s.clone(),
        _ => func.name.clone(),
    })
}

/// Escape a Rust string for embedding inside a double-quoted Koja
/// string literal in the generated harness source.
fn escape_koja_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The completion line for one trace-mode test.
///
/// In color mode the whole line is rewritten in the result color: a
/// leading `\r` returns to column 0 and the name + location + result
/// are reprinted colored, overwriting the uncolored pre-run anchor
/// (which stays put on a crash, preserving attribution). In no-color
/// mode the result is appended to the existing name line so piped
/// output carries no carriage returns.
fn trace_result_line(
    opts: TestOptions,
    escaped_desc: &str,
    location: &str,
    word: &str,
    color: &str,
    reset: &str,
) -> String {
    if opts.color {
        format!(
            "      IO.puts(\"\\r{color}  {escaped_desc} ({location}) ... {word} (#{{test_elapsed_ms}}ms){reset}\")\n"
        )
    } else {
        format!("      IO.puts(\" ... {word} (#{{test_elapsed_ms}}ms)\")\n")
    }
}

/// Generate the Koja source for the test harness file: a
/// [`HARNESS_ENTRY`] struct implementing `Process<(), (), ()>`
/// whose `run` executes the tests.
///
/// Each `@test` function returns some `Result<_, E>`. The idiom is a
/// unit `! String` or `! Test.Failure` body that passes by returning
/// and fails with `fail` or `assert`. Any success type works because
/// the harness only matches `Result.Ok(_)`, and any `E` works because
/// the failure text interpolates `msg`, which renders a `String` bare
/// and everything else through `Debug`. The harness calls each test
/// through its [`TestCase::call_path`], matches on the result to track
/// pass/fail counts, and continues running all tests even when some
/// fail. `run` stops with `StopReason.Shutdown` (exit 1) when any test
/// failed, `StopReason.Normal` (exit 0) otherwise.
///
/// Default output is a row of pass/fail dots followed by a summary.
/// [`TestOptions::trace`] swaps this for one header per
/// [`TestCase::group`] and one timed line per test: the test name and
/// `path:line` are
/// written first (no newline) so a crashing test leaves its name
/// dangling as the last output, then ` ... ok/FAIL (Nms)` is appended
/// once the test returns.
///
/// No imports are needed, since the gather-then-check pipeline makes
/// every project type visible to every file automatically.
pub fn generate_harness(tests: &[TestCase], opts: TestOptions) -> String {
    let (green, red, reset) = if opts.color {
        ("\x1b[32m", "\x1b[31m", "\x1b[0m")
    } else {
        ("", "", "")
    };

    let mut body = String::new();
    body.push_str("  failures: List<String> = []\n");
    body.push_str("  passed = 0\n");
    body.push_str("  failed = 0\n");

    let mut prev_group: Option<String> = None;
    for test in tests {
        let escaped_desc = escape_koja_string(&test.description);
        let location = escape_koja_string(&format!("{}:{}", test.file, test.line));
        let failure_append = format!(
            "      failures = failures.append(\"  #{{failed}}) {escaped_desc} ({location})\\n     #{{msg}}\")\n",
        );
        let call = test.call_path();

        if opts.trace {
            let group = test.group();
            if prev_group.as_deref() != Some(group.as_str()) {
                if prev_group.is_some() {
                    body.push_str("  IO.puts(\"\")\n");
                }
                body.push_str(&format!("  IO.puts(\"{}\")\n", escape_koja_string(&group)));
                prev_group = Some(group);
            }
            body.push_str(&format!("  IO.write(\"  {escaped_desc} ({location})\")\n"));
            body.push_str("  test_start_ms = DateTime.now().timestamp_millis()\n");
            body.push_str(&format!("  match {call}()\n"));
            body.push_str("    Result.Ok(_) ->\n");
            body.push_str("      passed = passed + 1\n");
            body.push_str(
                "      test_elapsed_ms = DateTime.now().timestamp_millis() - test_start_ms\n",
            );
            body.push_str(&trace_result_line(
                opts,
                &escaped_desc,
                &location,
                "ok",
                green,
                reset,
            ));
            body.push_str("    Result.Err(msg) ->\n");
            body.push_str("      failed = failed + 1\n");
            body.push_str(
                "      test_elapsed_ms = DateTime.now().timestamp_millis() - test_start_ms\n",
            );
            body.push_str(&trace_result_line(
                opts,
                &escaped_desc,
                &location,
                "FAIL",
                red,
                reset,
            ));
            body.push_str(&failure_append);
            body.push_str("  end\n");
        } else {
            body.push_str(&format!("  match {call}()\n"));
            body.push_str("    Result.Ok(_) ->\n");
            body.push_str("      passed = passed + 1\n");
            body.push_str(&format!("      IO.write(\"{green}.{reset}\")\n"));
            body.push_str("    Result.Err(msg) ->\n");
            body.push_str("      failed = failed + 1\n");
            body.push_str(&format!("      IO.write(\"{red}X{reset}\")\n"));
            body.push_str(&failure_append);
            body.push_str("  end\n");
        }
    }

    body.push_str("  IO.puts(\"\")\n");
    body.push_str("  if failed > 0\n");
    body.push_str("    IO.puts(\"\")\n");
    body.push_str("    IO.puts(\"Failures:\")\n");
    body.push_str("    IO.puts(\"\")\n");
    body.push_str("    for f in failures\n");
    body.push_str("      IO.puts(f)\n");
    body.push_str("      IO.puts(\"\")\n");
    body.push_str("    end\n");
    body.push_str(&format!(
        "    IO.puts(\"{red}#{{passed}} successful tests. #{{failed}} failures.{reset}\")\n"
    ));
    body.push_str("  else\n");
    body.push_str(&format!(
        "    IO.puts(\"{green}#{{passed}} successful tests. #{{failed}} failures.{reset}\")\n"
    ));
    body.push_str("  end\n");
    body.push_str("  cond\n");
    body.push_str("    failed > 0 -> Process.StopReason.Shutdown\n");
    body.push_str("    else -> Process.StopReason.Normal\n");
    body.push_str("  end\n");

    let mut source = String::new();
    source.push_str(&format!("struct {HARNESS_ENTRY}\nend\n\n"));
    source.push_str(&format!("impl Process<(), (), ()> for {HARNESS_ENTRY}\n"));
    source.push_str(&format!(
        "  fn start(config: ()) -> Self ! Process.StopReason\n    \
           {HARNESS_ENTRY}{{}}\n  \
         end\n\n"
    ));
    source.push_str(
        "  fn handle(self, msg: (), from: Option<ReplyTo<()>>) -> Process.Step<Self>\n    \
           Process.Step.Continue(self)\n  \
         end\n\n",
    );
    source.push_str("  fn run(self) -> Process.StopReason\n");
    source.push_str(&body);
    source.push_str("  end\nend\n");

    source
}
