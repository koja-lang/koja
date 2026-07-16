//! Shared scaffolding for the typecheck integration suite. Each
//! `tests/*.rs` file is a separate Cargo test binary, so helpers live
//! here behind a `mod common;`. The directory form (`tests/common/mod.rs`
//! rather than `tests/common.rs`) keeps Cargo from picking this up as a
//! test target itself.
//!
//! Four groups:
//!
//! - drivers: `typecheck_file` / `typecheck_script` (+ `_fail`) over a
//!   single in-memory source, `check_packages` / `check_multi_file` for
//!   multi-file programs
//! - failure assertions: `assert_fails_with` and its per-mode shorthands,
//!   plus `diagnostic_messages` / `warning_messages` for custom checks
//! - registry lookups: `global_leaf`, `package_leaf`, `global_named`,
//!   the primitive shorthands (`int_type`, ...), definitions and
//!   signatures
//! - AST navigation: `script_body`, `trailing_expr`,
//!   `trailing_resolution`, `find_function`, `function_body`

// Each test binary only pulls a subset of the helpers below, so
// `dead_code` would fire on every helper for every test that doesn't
// happen to use it. Silence it once at the module level.
#![allow(dead_code)]

use std::path::PathBuf;

use koja_ast::ast::{Expr, File, Function, Item, Severity, Statement, StructDecl};
use koja_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};
use koja_ast::util::dedent;
use koja_parser::{ParseMode, SourceFile, parse_program};
use koja_typecheck::{
    CheckFailure, CheckedProgram, EnumDefinition, FunctionSignature, GlobalKind, StructDefinition,
    check_program,
};

pub const PACKAGE: &str = "TestApp";

// ---------------------------------------------------------------------------
// Drivers
// ---------------------------------------------------------------------------

pub fn typecheck_file(source: &str) -> CheckedProgram {
    typecheck(source, ParseMode::File)
}

pub fn typecheck_file_fail(source: &str) -> CheckFailure {
    typecheck_fail(source, ParseMode::File)
}

pub fn typecheck_script(source: &str) -> CheckedProgram {
    typecheck(source, ParseMode::Script)
}

pub fn typecheck_script_fail(source: &str) -> CheckFailure {
    typecheck_fail(source, ParseMode::Script)
}

pub fn typecheck(source: &str, mode: ParseMode) -> CheckedProgram {
    parse_and_check(source, mode).unwrap_or_else(|failure| {
        panic!(
            "typecheck failed on `{source}`: {} diagnostic(s):\n{failure}",
            failure.diagnostics.len()
        )
    })
}

pub fn typecheck_fail(source: &str, mode: ParseMode) -> CheckFailure {
    parse_and_check(source, mode).expect_err(
        "expected typecheck to fail; it succeeded (test source must produce a diagnostic)",
    )
}

pub fn parse_and_check(source: &str, mode: ParseMode) -> Result<CheckedProgram, CheckFailure> {
    check_sources(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("test.koja"),
            source: source.to_string(),
        }],
        mode,
    )
}

/// Drive `parse_program -> check_program` on dedented `(package,
/// filename, body)` triples, so cross-package cases can stack a `Lib`
/// package next to `TestApp`.
pub fn check_packages(
    files: &[(&str, &str, &str)],
    mode: ParseMode,
) -> Result<CheckedProgram, CheckFailure> {
    let sources = files
        .iter()
        .map(|(package, name, body)| SourceFile {
            package: package.to_string(),
            path: PathBuf::from(name),
            source: dedent(body),
        })
        .collect();
    check_sources(sources, mode)
}

/// [`check_packages`] with every file in the test package. Used to
/// prove declarations reach sibling files inside one package.
pub fn check_multi_file(
    files: &[(&str, &str)],
    mode: ParseMode,
) -> Result<CheckedProgram, CheckFailure> {
    let stacked: Vec<(&str, &str, &str)> = files
        .iter()
        .map(|(name, body)| (PACKAGE, *name, *body))
        .collect();
    check_packages(&stacked, mode)
}

fn check_sources(
    user_sources: Vec<SourceFile>,
    mode: ParseMode,
) -> Result<CheckedProgram, CheckFailure> {
    let mut sources = koja_stdlib::autoimport_sources();
    sources.extend(koja_stdlib::qualified_sources());
    sources.extend(user_sources);
    check_program(parse_program(sources, mode))
}

// ---------------------------------------------------------------------------
// Failure assertions
// ---------------------------------------------------------------------------

pub fn diagnostic_messages(failure: &CheckFailure) -> Vec<String> {
    failure
        .diagnostics
        .iter()
        .map(|d| d.message.clone())
        .collect()
}

pub fn warning_messages(checked: &CheckedProgram) -> Vec<String> {
    checked
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .map(|d| d.message.clone())
        .collect()
}

/// Typecheck `source` (dedented) expecting failure, and assert every
/// needle appears in at least one diagnostic message.
pub fn assert_fails_with(source: &str, mode: ParseMode, needles: &[&str]) {
    let failure = typecheck_fail(&dedent(source), mode);
    let messages = diagnostic_messages(&failure);
    for needle in needles {
        assert!(
            messages.iter().any(|m| m.contains(needle)),
            "expected a diagnostic containing `{needle}`, got: {messages:#?}",
        );
    }
}

pub fn assert_file_fails_with(source: &str, needles: &[&str]) {
    assert_fails_with(source, ParseMode::File, needles);
}

pub fn assert_script_fails_with(source: &str, needles: &[&str]) {
    assert_fails_with(source, ParseMode::Script, needles);
}

// ---------------------------------------------------------------------------
// Registry lookups
// ---------------------------------------------------------------------------

pub fn registry_id(checked: &CheckedProgram, package: &str, path: &[&str]) -> GlobalRegistryId {
    let ident = Identifier::new(package, path.iter().map(|s| (*s).to_string()).collect());
    let (id, _) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("`{ident}` not found in registry"));
    id
}

pub fn global_id(checked: &CheckedProgram, name: &str) -> GlobalRegistryId {
    registry_id(checked, "Global", &[name])
}

pub fn global_leaf(checked: &CheckedProgram, name: &str) -> ResolvedType {
    ResolvedType::leaf(Resolution::Global(global_id(checked, name)))
}

pub fn package_leaf(checked: &CheckedProgram, name: &str) -> ResolvedType {
    ResolvedType::leaf(Resolution::Global(registry_id(checked, PACKAGE, &[name])))
}

/// `Global.<name><args>`, e.g. `global_named(&checked, "List", vec![...])`
/// for `List<T>`.
pub fn global_named(
    checked: &CheckedProgram,
    name: &str,
    type_args: Vec<ResolvedType>,
) -> ResolvedType {
    ResolvedType::Named {
        resolution: Resolution::Global(global_id(checked, name)),
        type_args,
    }
}

pub fn bool_type(checked: &CheckedProgram) -> ResolvedType {
    global_leaf(checked, "Bool")
}

pub fn float_type(checked: &CheckedProgram) -> ResolvedType {
    global_leaf(checked, "Float")
}

pub fn int_type(checked: &CheckedProgram) -> ResolvedType {
    global_leaf(checked, "Int")
}

pub fn never_type(checked: &CheckedProgram) -> ResolvedType {
    global_leaf(checked, "Never")
}

pub fn string_type(checked: &CheckedProgram) -> ResolvedType {
    global_leaf(checked, "String")
}

pub fn unit_type(checked: &CheckedProgram) -> ResolvedType {
    global_leaf(checked, "Unit")
}

pub fn assert_registered(checked: &CheckedProgram, segments: &[&str]) {
    let id = Identifier::new("Global", segments.iter().map(|s| s.to_string()).collect());
    assert!(
        checked.registry.lookup(&id).is_some(),
        "expected `{id}` to be registered after autoimport",
    );
}

pub fn struct_definition<'a>(checked: &'a CheckedProgram, name: &str) -> &'a StructDefinition {
    let ident = Identifier::new(PACKAGE, vec![name.to_string()]);
    let (_, entry) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("`{ident}` not found in registry"));
    match &entry.kind {
        GlobalKind::Struct(Some(definition)) => definition,
        other => panic!("expected lifted Struct(Some(_)) for `{ident}`, got {other:?}"),
    }
}

pub fn enum_definition<'a>(checked: &'a CheckedProgram, name: &str) -> &'a EnumDefinition {
    let ident = Identifier::new(PACKAGE, vec![name.to_string()]);
    let (_, entry) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("`{ident}` not found in registry"));
    match &entry.kind {
        GlobalKind::Enum(Some(definition)) => definition,
        other => panic!("expected lifted Enum(Some(_)) for `{ident}`, got {other:?}"),
    }
}

pub fn function_signature<'a>(
    checked: &'a CheckedProgram,
    package: &str,
    path: &[&str],
) -> &'a FunctionSignature {
    let ident = Identifier::new(package, path.iter().map(|s| (*s).to_string()).collect());
    let (_, entry) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("`{ident}` not found in registry"));
    match &entry.kind {
        GlobalKind::Function(Some(signature)) => signature,
        other => panic!("expected lifted Function(Some(_)) for `{ident}`, got {other:?}"),
    }
}

/// Lifted signature of `<type_name>.<method_name>` in the test package.
pub fn method_signature<'a>(
    checked: &'a CheckedProgram,
    type_name: &str,
    method_name: &str,
) -> &'a FunctionSignature {
    function_signature(checked, PACKAGE, &[type_name, method_name])
}

// ---------------------------------------------------------------------------
// AST navigation
// ---------------------------------------------------------------------------

/// The single user file of the test package.
pub fn test_file(checked: &CheckedProgram) -> &File {
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("checked program is missing the test package");
    pkg.files.first().expect("package has no files")
}

/// Top-level statements of a script-mode test file.
pub fn script_body(checked: &CheckedProgram) -> &[Statement] {
    test_file(checked)
        .body
        .as_deref()
        .expect("script-mode file must keep statements on File.body")
}

/// The trailing statement of `body`, which must be an expression.
pub fn last_expr(body: &[Statement]) -> &Expr {
    match body.last().expect("expected at least one statement") {
        Statement::Expr(expr) => expr,
        other => panic!("expected Statement::Expr as trailing statement, got {other:?}"),
    }
}

pub fn trailing_expr(checked: &CheckedProgram) -> &Expr {
    last_expr(script_body(checked))
}

pub fn trailing_resolution(checked: &CheckedProgram) -> ResolvedType {
    trailing_expr(checked).resolution.clone()
}

pub fn find_function<'a>(checked: &'a CheckedProgram, name: &str) -> &'a Function {
    test_file(checked)
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(function) if function.name == name => Some(function),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing fn `{name}`"))
}

pub fn function_body<'a>(checked: &'a CheckedProgram, name: &str) -> &'a [Statement] {
    find_function(checked, name)
        .body
        .as_deref()
        .unwrap_or_else(|| panic!("`{name}` has no body"))
}

pub fn find_struct_decl<'a>(checked: &'a CheckedProgram, name: &str) -> &'a StructDecl {
    for item in &test_file(checked).items {
        if let Item::Struct(decl) = item
            && decl.name() == name
        {
            return decl;
        }
    }
    panic!("struct `{name}` not found in checked program");
}
