//! Rewrites that run before every other pass so downstream code sees
//! one shape.
//!
//! - Lexically nested type declarations hoist to qualified top-level
//!   items, the same flat shape the qualified form
//!   (`struct Owner.Nested`) produces.
//! - `test "..."` blocks become functions with a `! Test.Failure`
//!   channel when the `Test` package is linked, and are dropped when it
//!   is not, so a build never type checks a test body. Top-level blocks
//!   become package-private functions. Blocks inside a struct, enum,
//!   impl, extend, or builtin body become public methods on that type.

use std::path::Path;

use koja_ast::ast::{
    File, Function, FunctionOrigin, ImplMember, Item, TestDecl, TypeExpr, Visibility,
    synthesized_test_name,
};

use crate::program::CheckedPackage;

const TEST_PACKAGE: &str = "Test";
const FAILURE_TYPE: &str = "Failure";

pub(crate) fn desugar_packages(packages: &mut [CheckedPackage]) {
    let tests_linked = packages.iter().any(|pkg| pkg.package == TEST_PACKAGE);
    for pkg in packages {
        for file in &mut pkg.files {
            desugar_tests(file, tests_linked);
            let mut items = Vec::with_capacity(file.items.len());
            for item in file.items.drain(..) {
                hoist_item(item, &mut items);
            }
            file.items = items;
        }
    }
}

/// Runs before hoisting so a nested struct's tests ride along with the
/// struct.
fn desugar_tests(file: &mut File, tests_linked: bool) {
    let path = file.path.clone();
    let mut items = Vec::with_capacity(file.items.len());
    for item in file.items.drain(..) {
        match item {
            Item::Test(test) => {
                if tests_linked {
                    items.push(Item::Function(test_function(
                        test,
                        path.as_deref(),
                        Visibility::Private,
                    )));
                }
            }
            mut item => {
                desugar_nested_tests(
                    std::slice::from_mut(&mut item),
                    path.as_deref(),
                    tests_linked,
                );
                items.push(item);
            }
        }
    }
    file.items = items;
}

/// Member tests stay public. `priv` on a method is type-private, which
/// would hide the test from the harness spliced into the package.
fn desugar_nested_tests(items: &mut [Item], path: Option<&Path>, tests_linked: bool) {
    for item in items {
        match item {
            Item::Builtin(decl) => {
                member_tests(&mut decl.tests, &mut decl.functions, path, tests_linked);
            }
            Item::Enum(decl) => {
                member_tests(&mut decl.tests, &mut decl.functions, path, tests_linked);
                desugar_nested_tests(&mut decl.nested, path, tests_linked);
            }
            Item::Extend(block) => {
                impl_member_tests(&mut block.tests, &mut block.members, path, tests_linked);
            }
            Item::Impl(block) => {
                impl_member_tests(&mut block.tests, &mut block.members, path, tests_linked);
            }
            Item::Struct(decl) => {
                member_tests(&mut decl.tests, &mut decl.functions, path, tests_linked);
                desugar_nested_tests(&mut decl.nested, path, tests_linked);
            }
            _ => {}
        }
    }
}

fn member_tests(
    tests: &mut Vec<TestDecl>,
    functions: &mut Vec<Function>,
    path: Option<&Path>,
    tests_linked: bool,
) {
    let tests = std::mem::take(tests);
    if tests_linked {
        functions.extend(
            tests
                .into_iter()
                .map(|test| test_function(test, path, Visibility::Public)),
        );
    }
}

fn impl_member_tests(
    tests: &mut Vec<TestDecl>,
    members: &mut Vec<ImplMember>,
    path: Option<&Path>,
    tests_linked: bool,
) {
    let tests = std::mem::take(tests);
    if tests_linked {
        members.extend(
            tests
                .into_iter()
                .map(|test| ImplMember::Function(test_function(test, path, Visibility::Public))),
        );
    }
}

fn test_function(test: TestDecl, path: Option<&Path>, visibility: Visibility) -> Function {
    let span = test.span;
    Function {
        annotations: Vec::new(),
        origin: FunctionOrigin::Test,
        visibility,
        name: synthesized_test_name(path, span.start.line),
        type_params: Vec::new(),
        params: Vec::new(),
        return_type: None,
        error_type: Some(TypeExpr::Named {
            path: vec![TEST_PACKAGE.to_string(), FAILURE_TYPE.to_string()],
            span,
        }),
        body: Some(test.body),
        span,
    }
}

fn hoist_item(mut item: Item, out: &mut Vec<Item>) {
    let (owner_path, nested) = match &mut item {
        Item::Enum(decl) => (decl.path.clone(), std::mem::take(&mut decl.nested)),
        Item::Struct(decl) => (decl.path.clone(), std::mem::take(&mut decl.nested)),
        _ => {
            out.push(item);
            return;
        }
    };
    out.push(item);
    for mut nested_item in nested {
        match &mut nested_item {
            Item::Enum(decl) => prefix_path(&mut decl.path, &owner_path),
            Item::Struct(decl) => prefix_path(&mut decl.path, &owner_path),
            _ => {}
        }
        hoist_item(nested_item, out);
    }
}

fn prefix_path(path: &mut Vec<String>, owner: &[String]) {
    path.splice(0..0, owner.iter().cloned());
}
