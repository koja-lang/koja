//! Test discovery and harness synthesis for `koja test`.
//!
//! The driver feeds a parsed project (sources + test fixtures) into
//! [`discover_tests`] to enumerate every `@test`-annotated function
//! belonging to the current project. [`generate_harness`] then
//! produces an Koja source string for a synthetic
//! [`HARNESS_ENTRY`] type implementing `Process<(), (), ()>` whose
//! `run` invokes each test, tracks pass/fail counts, and stops with
//! `StopReason.Shutdown` (exit 1) when anything fails. The driver
//! splices that harness into the parsed program and lowers with
//! [`HARNESS_ENTRY`] as the project's Process entry.
//!
//! Kept backend-agnostic on purpose: this crate only depends on
//! the AST + parser surface so both the pipeline and (any
//! future) v1 fallback can share the same harness shape.

use std::path::Path;

use koja_ast::ast::{AnnotationValue, Item};
use koja_parser::ParsedProgram;

/// Name of the synthesized test-harness entry type. Reserved for
/// the test runner; the driver passes this as the project's
/// Process entry when lowering test builds, so it must match the
/// struct name emitted by [`generate_harness`].
pub const HARNESS_ENTRY: &str = "KojaTestHarness";

/// Output knobs for the synthesized harness.
///
/// `trace` swaps the compact dots-and-summary output for one group
/// header per struct and one timed line per test (modeled on
/// `mix test --trace`); `color` gates the ANSI escapes so
/// `--no-color` / `NO_COLOR` reach the generated source.
#[derive(Clone, Copy, Debug, Default)]
pub struct TestOptions {
    pub color: bool,
    pub trace: bool,
}

/// A discovered `@test` function inside a struct, called as
/// `StructName.fn_name()` from the generated harness. `file` and
/// `line` record the source location (`file` is rendered relative
/// to the project root) for navigable trace and failure output.
#[derive(Clone, Debug)]
pub struct TestCase {
    pub description: String,
    pub file: String,
    pub fn_name: String,
    pub line: u32,
    pub struct_name: String,
}

/// Walks the parsed program and collects `@test`-annotated functions
/// inside structs. Only scans files belonging to the current project
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

        for item in &file.ast.items {
            let Item::Struct(s) = item else {
                continue;
            };
            for func in &s.functions {
                let Some(ann) = func.annotations.iter().find(|a| a.name == "test") else {
                    continue;
                };
                let description = match &ann.value {
                    Some(AnnotationValue::String(s)) => s.clone(),
                    _ => func.name.clone(),
                };
                tests.push(TestCase {
                    description,
                    file: display_path.clone(),
                    fn_name: func.name.clone(),
                    line: func.span.start.line,
                    struct_name: s.name().to_string(),
                });
            }
        }
    }

    tests
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
/// Each `@test` function must return `Result<Bool, String>`. The
/// harness calls each test as `StructName.fn_name()`, matches on the
/// result to track pass/fail counts, and continues running all tests
/// even when some fail. `run` stops with `StopReason.Shutdown`
/// (exit 1) when any test failed, `StopReason.Normal` (exit 0)
/// otherwise.
///
/// Default output is a row of pass/fail dots followed by a summary.
/// [`TestOptions::trace`] swaps this for one group header per struct
/// and one timed line per test: the test name and `path:line` are
/// written first (no newline) so a crashing test leaves its name
/// dangling as the last output, then ` ... ok/FAIL (Nms)` is appended
/// once the test returns.
///
/// No imports are needed — the gather-then-check pipeline makes
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

    let mut prev_struct: Option<&str> = None;
    for test in tests {
        let escaped_desc = escape_koja_string(&test.description);
        let location = escape_koja_string(&format!("{}:{}", test.file, test.line));
        let failure_append = format!(
            "      failures = failures.append(\"  #{{failed}}) {escaped_desc} ({location})\\n     \" <> msg)\n",
        );

        if opts.trace {
            if prev_struct != Some(test.struct_name.as_str()) {
                if prev_struct.is_some() {
                    body.push_str("  IO.puts(\"\")\n");
                }
                body.push_str(&format!("  IO.puts(\"{}\")\n", test.struct_name));
                prev_struct = Some(test.struct_name.as_str());
            }
            body.push_str(&format!("  IO.write(\"  {escaped_desc} ({location})\")\n"));
            body.push_str("  test_start_ms = DateTime.now().timestamp_millis()\n");
            body.push_str(&format!(
                "  match {}.{}()\n",
                test.struct_name, test.fn_name
            ));
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
            body.push_str(&format!(
                "  match {}.{}()\n",
                test.struct_name, test.fn_name
            ));
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
    body.push_str("    failed > 0 -> StopReason.Shutdown\n");
    body.push_str("    else -> StopReason.Normal\n");
    body.push_str("  end\n");

    let mut source = String::new();
    source.push_str(&format!("struct {HARNESS_ENTRY}\nend\n\n"));
    source.push_str(&format!("impl Process<(), (), ()> for {HARNESS_ENTRY}\n"));
    source.push_str(&format!(
        "  fn start(config: ()) -> Result<Self, StopReason>\n    \
           Result.Ok({HARNESS_ENTRY}{{}})\n  \
         end\n\n"
    ));
    source.push_str(
        "  fn handle(self, msg: (), from: Option<ReplyTo<()>>) -> Step<Self>\n    \
           Step.Continue(self)\n  \
         end\n\n",
    );
    source.push_str("  fn run(self) -> StopReason\n");
    source.push_str(&body);
    source.push_str("  end\nend\n");

    source
}
