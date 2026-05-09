//! Typecheck coverage for `while` and `for`.
//!
//! `while` pins: `Bool` condition, body resolves under the enclosing
//! scope, mutable bindings propagate, expression resolves to `Unit`.
//!
//! `for pat in iter ... end` runs through `synthesize::for_desugar`
//! before resolve. The contract is nominal: `iter.get` must return
//! `Global.Option<T>` exactly. Diagnostics come from the normal
//! method-lookup / constructor-pattern paths (no `for`-specific
//! validator).

use expo_alpha_typecheck::CheckedProgram;
use expo_ast::ast::{Item, Statement};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::util::dedent;

mod common;

use common::{
    PACKAGE, diagnostic_messages, typecheck_file as typecheck,
    typecheck_file_fail as typecheck_fail,
};

/// `Enumeration<Int>` fixture using stdlib `Option<Int>`. `get`
/// returns `Some(...)` unconditionally — the desugar's `__idx <
/// __len` guard ensures it's only called for valid indices, and a
/// literal `None` branch needs return-type back-propagation into
/// unit-variant inference (orthogonal feature gap).
const ENUMERABLE_FIXTURE: &str = "
    struct Counter
      start: Int
      finish: Int
    end

    impl Counter
      fn length(self) -> Int
        self.finish - self.start
      end

      fn get(self, index: Int) -> Option<Int>
        Option.Some(self.start + index)
      end
    end
    ";

fn trailing_resolution(checked: &CheckedProgram) -> ResolvedType {
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("checked program is missing the test package");
    let file = pkg.files.first().expect("package has no files");
    let main = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(function) if function.name == "main" => Some(function),
            _ => None,
        })
        .expect("file is missing `fn main`");
    let body = main
        .body
        .as_deref()
        .expect("`fn main` has no body — extern fn cannot be the entry point");
    let trailing = body.last().expect("expected at least one statement");
    match trailing {
        Statement::Expr(expr) => expr.resolution.clone(),
        other => panic!("expected Statement::Expr as trailing statement, got {other:?}"),
    }
}

fn primitive_type(checked: &CheckedProgram, name: &str) -> ResolvedType {
    let ident = Identifier::new("Global", vec![name.to_string()]);
    let (id, _) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("stdlib stub `Global.{name}` missing from registry"));
    ResolvedType::leaf(Resolution::Global(id))
}

fn unit_type(checked: &CheckedProgram) -> ResolvedType {
    primitive_type(checked, "Unit")
}

#[test]
fn while_with_bool_condition_resolves_to_unit() {
    let source = "
        fn main
          i = 0
          while i < 3
            i = i + 1
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), unit_type(&checked));
}

#[test]
fn while_with_int_condition_diagnoses() {
    let source = "
        fn main
          while 1
            2
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("`while` condition must be `Bool`")),
        "expected `while` condition diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn while_body_assignment_propagates_local_type() {
    // Mutable bindings inside the body must resolve through the
    // same `LocalScope::declare` path as anywhere else; subsequent
    // reads see the same `LocalId`.
    let source = "
        fn main
          i = 0
          sum = 0
          while i < 10
            sum = sum + i
            i = i + 1
          end
          sum
        end
        ";
    let checked = typecheck(&dedent(source));
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("missing test package");
    let file = pkg.files.first().expect("package has no files");
    let main = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(f) if f.name == "main" => Some(f),
            _ => None,
        })
        .expect("missing `fn main`");
    let body = main.body.as_deref().expect("`fn main` has no body");
    let int_ty = primitive_type(&checked, "Int");
    // Trailing `sum` reads the body-mutated local — its resolution
    // is `Int`, proving the body's writes propagated.
    let trailing = body.last().expect("missing trailing");
    let Statement::Expr(expr) = trailing else {
        panic!("expected trailing Statement::Expr");
    };
    assert_eq!(expr.resolution, int_ty);
}

#[test]
fn while_with_string_condition_diagnoses() {
    let source = "
        fn main
          while \"yes\"
            1
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("`while` condition must be `Bool`")),
        "expected `while` condition diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

fn with_fixture(body: &str) -> String {
    format!("{ENUMERABLE_FIXTURE}\n{body}")
}

#[test]
fn for_over_enumerable_resolves_to_unit_and_binds_int() {
    // The Some-arm binds `x: Int`, so the body's `sum + x`
    // typechecks; trailing `sum` proves the binding flowed.
    let source = with_fixture(
        "
        fn main
          c = Counter{start: 10, finish: 13}
          sum = 0
          for x in c
            sum = sum + x
          end
          sum
        end
        ",
    );
    let checked = typecheck(&dedent(&source));
    assert_eq!(
        trailing_resolution(&checked),
        primitive_type(&checked, "Int")
    );
}

#[test]
fn for_with_wildcard_pattern_typechecks() {
    // `_` skips binding. The body still needs to resolve, but
    // there's no binding to consult.
    let source = with_fixture(
        "
        fn main
          c = Counter{start: 0, finish: 5}
          count = 0
          for _ in c
            count = count + 1
          end
          count
        end
        ",
    );
    let checked = typecheck(&dedent(&source));
    assert_eq!(
        trailing_resolution(&checked),
        primitive_type(&checked, "Int")
    );
}

#[test]
fn for_over_int_diagnoses_missing_length() {
    // `Int` is a Global struct stub with no `length` method, so
    // the desugar's `__it.length()` call fails to resolve.
    let source = "
        fn main
          for x in 5
            x
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("has no method `length")),
        "expected missing-`length` diagnostic, got: {messages:?}",
    );
}

#[test]
fn for_over_struct_without_length_diagnoses() {
    let source = "
        struct Bare
          x: Int
        end

        fn main
          b = Bare{x: 1}
          for v in b
            v
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("has no method `length")),
        "expected missing-`length` diagnostic, got: {messages:?}",
    );
}

#[test]
fn for_with_get_returning_non_enum_diagnoses() {
    // `get` returns `Int`; the desugar's `match` constructor
    // shorthand needs an enum subject.
    let source = "
        struct Bad
          x: Int
        end

        impl Bad
          fn length(self) -> Int
            1
          end

          fn get(self, index: Int) -> Int
            self.x
          end
        end

        fn main
          b = Bad{x: 7}
          for v in b
            v
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("requires an enum subject")),
        "expected non-enum subject diagnostic, got: {messages:?}",
    );
}

#[test]
fn for_with_get_returning_wrong_enum_diagnoses_missing_some_none() {
    // The desugar matches `Some` / `None` on the subject enum; an
    // enum without those variants (here `Present` / `Absent`)
    // flunks the constructor-shorthand variant lookup.
    let source = "
        enum NotOption
          Present(Int)
          Absent
        end

        struct Wrong
          x: Int
        end

        impl Wrong
          fn length(self) -> Int
            1
          end

          fn get(self, index: Int) -> NotOption
            NotOption.Present(self.x)
          end
        end

        fn main
          w = Wrong{x: 0}
          for v in w
            v
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("no variant `Some`")),
        "expected missing-`Some` diagnostic, got: {messages:?}",
    );
    assert!(
        messages.iter().any(|m| m.contains("no variant `None`")),
        "expected missing-`None` diagnostic, got: {messages:?}",
    );
}
