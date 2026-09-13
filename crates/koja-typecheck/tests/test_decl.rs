//! Typecheck pins for the `test "..."` declaration.
//!
//! - With `Test` linked, a top-level test becomes a package-private
//!   function and a struct test a public method, both named
//!   `__test_<stem>_<line>` with `FunctionOrigin::Test` and a
//!   `! Test.Failure` channel.
//! - Without `Test`, both are dropped and the program still checks.
//! - A body that breaks the channel reports inside the test block.

use std::path::PathBuf;

use koja_ast::ast::{FunctionOrigin, Item, TypeExpr, Visibility, synthesized_test_name};
use koja_ast::util::dedent;
use koja_parser::{ParseMode, SourceFile, parse_program};
use koja_typecheck::check_program;

mod common;

use common::{PACKAGE, find_function, find_struct_decl, test_file, typecheck_file};

const SOURCE: &str = "
    struct Stack
      items: List<Int>

      fn push(self, item: Int) -> Stack
        Stack{items: self.items.append(item)}
      end

      test \"push grows the stack\"
        stack = Stack{items: List.new()}
        assert stack.push(1).items.length() == 1
      end
    end

    test \"a top-level test\"
      assert 1 == 1
    end
    ";

fn check_without_test_package(source: &str) -> koja_typecheck::CheckedProgram {
    let mut sources = koja_stdlib::autoimport_sources();
    sources.extend(koja_stdlib::qualified_sources_for(false));
    sources.push(SourceFile {
        package: PACKAGE.to_string(),
        path: PathBuf::from("test.koja"),
        source: dedent(source),
    });
    check_program(parse_program(sources, ParseMode::File))
        .unwrap_or_else(|failure| panic!("expected the program to check:\n{failure}"))
}

fn assert_test_function(function: &koja_ast::ast::Function, line: u32, visibility: Visibility) {
    assert_eq!(
        function.name,
        synthesized_test_name(Some(&PathBuf::from("test.koja")), line)
    );
    assert_eq!(function.name, format!("__test_test_{line}"));
    assert_eq!(function.origin, FunctionOrigin::Test);
    assert_eq!(function.visibility, visibility);
    assert!(function.params.is_empty());
    assert!(function.return_type.is_none());
    let Some(TypeExpr::Named { path, .. }) = &function.error_type else {
        panic!(
            "expected a named error channel, got {:?}",
            function.error_type
        );
    };
    assert_eq!(path, &["Test", "Failure"]);
    assert_eq!(function.span.start.line, line);
}

#[test]
fn top_level_test_becomes_a_private_function() {
    let checked = typecheck_file(&dedent(SOURCE));
    let function = find_function(&checked, "__test_test_14");
    assert_test_function(function, 14, Visibility::Private);
    assert!(
        !test_file(&checked)
            .items
            .iter()
            .any(|item| matches!(item, Item::Test(_))),
        "no Item::Test survives desugar"
    );
}

#[test]
fn struct_test_becomes_a_private_method() {
    let checked = typecheck_file(&dedent(SOURCE));
    let stack = find_struct_decl(&checked, "Stack");
    assert!(stack.tests.is_empty(), "tests drain into functions");
    let function = stack
        .functions
        .iter()
        .find(|f| f.origin == FunctionOrigin::Test)
        .expect("struct test desugared into a method");
    assert_test_function(function, 8, Visibility::Public);
    assert_eq!(stack.functions.len(), 2);
}

#[test]
fn nested_struct_test_rides_the_hoist() {
    let checked = typecheck_file(&dedent(
        "
        struct Outer
          struct Inner
            value: Int

            test \"inner has a value\"
              assert Outer.Inner{value: 1}.value == 1
            end
          end
        end
        ",
    ));
    let inner = test_file(&checked)
        .items
        .iter()
        .find_map(|item| match item {
            Item::Struct(decl) if decl.path == ["Outer", "Inner"] => Some(decl),
            _ => None,
        })
        .expect("Outer.Inner hoisted to the top level");
    assert_eq!(inner.functions.len(), 1);
    assert_test_function(&inner.functions[0], 5, Visibility::Public);
}

#[test]
fn without_the_test_package_tests_are_dropped() {
    let checked = check_without_test_package(SOURCE);
    let file = test_file(&checked);
    assert!(
        !file.items.iter().any(|item| matches!(item, Item::Test(_))),
        "top-level test stripped"
    );
    assert!(
        !file
            .items
            .iter()
            .any(|item| matches!(item, Item::Function(f) if f.origin == FunctionOrigin::Test)),
        "no synthesized function without Test"
    );
    let stack = find_struct_decl(&checked, "Stack");
    assert!(stack.tests.is_empty());
    assert_eq!(stack.functions.len(), 1);
}

#[test]
fn body_that_breaks_the_channel_reports_inside_the_block() {
    let failure = common::typecheck_file_fail(&dedent(
        "
        test \"wrong failure type\"
          fail 1
        end
        ",
    ));
    let diagnostic = failure
        .diagnostics
        .iter()
        .find(|d| d.message.contains("declares error type `Failure`"))
        .unwrap_or_else(|| {
            panic!(
                "expected a channel mismatch, got {:#?}",
                failure.diagnostics
            )
        });
    let line = diagnostic.span.start.line;
    assert!(
        (1..=3).contains(&line),
        "reported inside the block, got line {line}"
    );
}
