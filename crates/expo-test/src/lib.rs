//! Test discovery and harness synthesis for `expo test`.
//!
//! The driver feeds a parsed project (sources + test fixtures) into
//! [`discover_tests`] to enumerate every `@test`-annotated function
//! belonging to the current project. [`generate_harness`] then
//! produces an Expo source string for a synthetic
//! `fn __expo_test_entry` that invokes each test, tracks pass/fail
//! counts, and exits non-zero when anything fails. The driver
//! splices that harness into the parsed program and lowers with
//! [`HARNESS_ENTRY`] as the project entry, so the user's own
//! `fn main` (if any) coexists as dead code in the test binary
//! without colliding on the `main` name.
//!
//! Kept backend-agnostic on purpose: this crate only depends on
//! the AST + parser surface so both the pipeline and (any
//! future) v1 fallback can share the same harness shape.

use expo_ast::ast::{AnnotationValue, Item};
use expo_parser::ParsedProgram;

/// Name of the synthesized test-harness entry function. Reserved
/// for the test runner; the driver passes this as the project
/// entry when lowering test builds, so it must match the function
/// name emitted by [`generate_harness`].
pub const HARNESS_ENTRY: &str = "__expo_test_entry";

/// A discovered `@test` function inside a struct, called as
/// `StructName.fn_name()` from the generated harness.
#[derive(Clone, Debug)]
pub struct TestCase {
    pub description: String,
    pub fn_name: String,
    pub struct_name: String,
}

/// Walks the parsed program and collects `@test`-annotated functions
/// inside structs. Only scans files belonging to the current project
/// (matched by the per-file `package` field), so deps' fixtures don't
/// sneak into the harness.
pub fn discover_tests(parsed: &ParsedProgram, project_name: &str) -> Vec<TestCase> {
    let mut tests = Vec::new();

    for file in parsed.iter() {
        if file.package != project_name {
            continue;
        }

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
                    fn_name: func.name.clone(),
                    struct_name: s.name.clone(),
                });
            }
        }
    }

    tests
}

/// Generate the Expo source for the test harness file.
///
/// Each `@test` function must return `Result<Bool, String>`. The
/// harness calls each test as `StructName.fn_name()`, matches on the
/// result to track pass/fail counts, and continues running all tests
/// even when some fail. A final non-zero exit (via `Kernel.exit(1)`)
/// is triggered when any test failed.
///
/// No imports are needed — the gather-then-check pipeline makes
/// every project type visible to every file automatically.
pub fn generate_harness(tests: &[TestCase]) -> String {
    let green = "\x1b[32m";
    let red = "\x1b[31m";
    let reset = "\x1b[0m";

    let mut body = String::new();
    body.push_str("  failures: List<String> = []\n");
    body.push_str("  passed = 0\n");
    body.push_str("  failed = 0\n");

    for test in tests {
        let escaped_desc = test.description.replace('\\', "\\\\").replace('"', "\\\"");
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
        body.push_str(&format!(
            "      failures = failures.append(\"  #{{failed}}) {} ({})\\n     \" <> msg)\n",
            escaped_desc, test.struct_name
        ));
        body.push_str("  end\n");
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
    body.push_str("    Kernel.exit(1)\n");
    body.push_str("  else\n");
    body.push_str(&format!(
        "    IO.puts(\"{green}#{{passed}} successful tests. #{{failed}} failures.{reset}\")\n"
    ));
    body.push_str("  end\n");

    let mut source = String::new();
    source.push_str(&format!("fn {HARNESS_ENTRY}\n"));
    source.push_str(&body);
    source.push_str("end\n");

    source
}
