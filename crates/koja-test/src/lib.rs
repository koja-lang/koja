//! Test discovery and harness synthesis for `koja test`.
//!
//! The driver feeds a parsed project (sources + test fixtures) into
//! [`discover_tests`] to enumerate every `test "..."` block and
//! `@test`-annotated function belonging to the current project.
//! [`generate_harness`] then
//! produces a Koja source string for a synthetic
//! [`HARNESS_ENTRY`] type implementing
//! `Process<(), Process.ExitSignal, ()>` whose `run` registers every
//! test as a `Test.Spec`, hands the plan to `Test.run`, and maps the
//! runner's exit into `StopReason`. Running and reporting live in the
//! `Test` package (`lib/test`). The driver splices the harness into
//! the parsed program and lowers with [`HARNESS_ENTRY`] as the
//! project's Process entry.
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

/// A discovered test, registered as `Outer.Inner.fn_name` or bare
/// `fn_name` by the generated harness. `file` and `line` record the
/// source location (`file` is rendered relative to the project root)
/// for navigable trace and failure output.
#[derive(Clone, Debug)]
pub struct TestCase {
    pub description: String,
    pub file: String,
    pub fn_name: String,
    pub line: u32,
    /// `None` for a top-level `test` block.
    pub owner: Option<Owner>,
    pub shape: Shape,
}

/// How the harness turns a test function into a `Test.Spec.run`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// The function already is `fn () -> Result<(), Test.Failure>`,
    /// so the harness registers a reference to it. Every `test` block
    /// and every `@test` function on a `Test.Failure` channel.
    Spec,
    /// An `@test` function on some other `Result<_, E>`. The harness
    /// wraps the call in a closure that keeps `Ok` and turns `Err(e)`
    /// into `Test.Failure.Error` with `e` interpolated, so a `String`
    /// renders bare and everything else through `Debug`.
    Legacy,
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
    /// The qualified function the harness registers.
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
            package: project_name,
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
    package: &'a str,
    path: Option<&'a Path>,
    tests: &'a mut Vec<TestCase>,
}

impl Collector<'_> {
    fn push(&mut self, description: String, owner: Option<&Owner>, line: u32) {
        let fn_name = synthesized_test_name(self.path, line);
        self.push_named(description, fn_name, owner, line, Shape::Spec);
    }

    fn push_named(
        &mut self,
        description: String,
        fn_name: String,
        owner: Option<&Owner>,
        line: u32,
        shape: Shape,
    ) {
        self.tests.push(TestCase {
            description,
            file: self.file.to_string(),
            fn_name,
            line,
            owner: owner.cloned(),
            shape,
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
                            annotated_test_shape(func, self.package),
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

/// Whether an `@test` function already has the `Test.Spec.run` shape,
/// a unit success value on a `Test.Failure` channel. Inside the `Test`
/// package itself the channel is spelled `Failure`.
fn annotated_test_shape(func: &Function, package: &str) -> Shape {
    let unit_success = match &func.return_type {
        None => true,
        Some(TypeExpr::Unit { .. }) => true,
        Some(_) => false,
    };
    let failure_channel = match func.error_type.as_ref().and_then(type_expr_path) {
        Some(path) => path == ["Test", "Failure"] || (package == "Test" && path == ["Failure"]),
        None => false,
    };
    if unit_success && failure_channel {
        Shape::Spec
    } else {
        Shape::Legacy
    }
}

/// Escape a Rust string for embedding inside a double-quoted Koja
/// string literal in the generated harness source.
fn escape_koja_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The `run` field of one registered spec. A reference to the test
/// function when it already has the spec shape, or a closure that
/// adapts a legacy result.
fn spec_run(test: &TestCase) -> String {
    let call = test.call_path();
    match test.shape {
        Shape::Spec => format!("&{call}/0"),
        Shape::Legacy => format!(
            r##"fn () -> Result<(), Test.Failure>
        match {call}()
          Result.Ok(_) -> Result.Ok(())
          Result.Err(error) -> Result.Err(Test.Failure.Error("#{{error}}"))
        end
      end"##
        ),
    }
}

/// Generate the Koja source for the test harness file: a
/// [`HARNESS_ENTRY`] struct implementing
/// `Process<(), Process.ExitSignal, ()>` whose `run` registers the
/// tests and waits for the runner.
///
/// Each test becomes one `Test.Spec` with its description, location,
/// group (the owner type, or the file for a top-level block), and a
/// `run` built by [`spec_run`]. The harness hands the plan to
/// `Test.run` with `Test.Options.from_env`, so every flag the driver
/// resolved arrives through `KOJA_TEST_*` variables, then monitors
/// the runner and maps its exit to the process result. A `Normal`
/// exit means every spec passed or was skipped, and anything else is
/// `StopReason.Shutdown` (exit 1).
///
/// No imports are needed, since the gather-then-check pipeline makes
/// every project type visible to every file automatically, and the
/// `Test` package is linked into every test build.
pub fn generate_harness(tests: &[TestCase]) -> String {
    let mut specs = String::new();
    for test in tests {
        specs.push_str(&format!(
            r#"      Test.Spec{{
        description: "{description}",
        file: "{file}",
        group: "{group}",
        line: {line},
        run: {run},
      }},
"#,
            description = escape_koja_string(&test.description),
            file = escape_koja_string(&test.file),
            group = escape_koja_string(&test.group()),
            line = test.line,
            run = spec_run(test),
        ));
    }

    format!(
        r#"struct {HARNESS_ENTRY}
end

impl Process<(), Process.ExitSignal, ()> for {HARNESS_ENTRY}
  fn start(config: ()) -> Self ! Process.StopReason
    {HARNESS_ENTRY}{{}}
  end

  fn handle(self, msg: Process.ExitSignal, from: Option<ReplyTo<()>>) -> Process.Step<Self>
    Process.Step.Continue(self)
  end

  fn run(self) -> Process.StopReason
    specs: List<Test.Spec> = [
{specs}    ]
    runner = Test.run(Test.Plan{{specs: specs}}, Test.Options.from_env())
    Process.monitor(runner.pid())

    receive
      envelope: (Process.ExitSignal, Option<ReplyTo<()>>) ->
        (signal, _) = envelope

        match signal.reason
          Process.ExitReason.Normal -> Process.StopReason.Normal
          _ -> Process.StopReason.Shutdown
        end

      event: Process.Lifecycle ->
        Process.StopReason.Shutdown
    end
  end
end
"#
    )
}
