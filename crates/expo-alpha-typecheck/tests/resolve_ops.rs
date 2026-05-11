//! Typecheck coverage for boolean and comparison operators
//! (`and`/`or`/`not`/`== != < > <= >=`) — pairs with
//! `pipeline::resolve::ops` in src. Mirrors `program.rs`: parse +
//! check a tiny `fn main`, then inspect the trailing expression's
//! `resolution`. Error paths assert a diagnostic on ill-typed
//! programs.

use expo_alpha_typecheck::CheckedProgram;
use expo_ast::ast::{Item, Statement};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};

mod common;

use common::{PACKAGE, typecheck_file as typecheck, typecheck_file_fail as typecheck_fail};

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

/// Resolved leaf for the preloaded `Global.<name>` stdlib stub.
fn global_leaf(checked: &CheckedProgram, name: &str) -> ResolvedType {
    let ident = Identifier::new("Global", vec![name.to_string()]);
    let (id, _) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("stdlib stub `Global.{name}` missing from registry"));
    ResolvedType::leaf(Resolution::Global(id))
}

fn bool_type(checked: &CheckedProgram) -> ResolvedType {
    global_leaf(checked, "Bool")
}

fn float_type(checked: &CheckedProgram) -> ResolvedType {
    global_leaf(checked, "Float")
}

fn int_type(checked: &CheckedProgram) -> ResolvedType {
    global_leaf(checked, "Int")
}

fn assert_trailing_is(source: &str, expected_name: &str) {
    let checked = typecheck(source);
    let expected = global_leaf(&checked, expected_name);
    let actual = trailing_resolution(&checked);
    assert_eq!(
        actual, expected,
        "source = {source:?} did not resolve to Global.{expected_name}",
    );
}

#[test]
fn logical_and_or_resolve_to_bool() {
    assert_trailing_is("fn main\n  true and false\nend\n", "Bool");
    assert_trailing_is("fn main\n  true or false\nend\n", "Bool");
}

#[test]
fn unary_not_resolves_to_bool() {
    assert_trailing_is("fn main\n  not true\nend\n", "Bool");
}

#[test]
fn unary_neg_resolves_to_int() {
    assert_trailing_is("fn main\n  -7\nend\n", "Int");
}

#[test]
fn comparisons_resolve_to_bool() {
    for source in [
        "fn main\n  1 == 1\nend\n",
        "fn main\n  1 != 2\nend\n",
        "fn main\n  1 < 2\nend\n",
        "fn main\n  1 > 2\nend\n",
        "fn main\n  1 <= 2\nend\n",
        "fn main\n  1 >= 2\nend\n",
    ] {
        let checked = typecheck(source);
        assert_eq!(
            trailing_resolution(&checked),
            bool_type(&checked),
            "source = {source:?}",
        );
    }
}

#[test]
fn bool_equality_is_allowed() {
    let checked = typecheck("fn main\n  true == false\nend\n");
    assert_eq!(trailing_resolution(&checked), bool_type(&checked));
}

#[test]
fn int_type_helper_still_references_int() {
    // Sanity check that both `int_type` and `bool_type` correspond to
    // the stubs the resolver emits; catches reverse-index breakage.
    let checked = typecheck("fn main\n  1 + 1\nend\n");
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
    assert_ne!(int_type(&checked), bool_type(&checked));
}

#[test]
fn mixed_int_and_bool_and_diagnoses() {
    let failure = typecheck_fail("fn main\n  1 and true\nend\n");
    assert_eq!(failure.diagnostics.len(), 1);
    assert!(
        failure.diagnostics[0].message.contains("`and`"),
        "unexpected diagnostic: {}",
        failure.diagnostics[0].message,
    );
}

#[test]
fn ordering_on_bool_diagnoses() {
    let failure = typecheck_fail("fn main\n  true < false\nend\n");
    assert_eq!(failure.diagnostics.len(), 1);
    assert!(
        failure.diagnostics[0].message.contains("Int or Float"),
        "unexpected diagnostic: {}",
        failure.diagnostics[0].message,
    );
}

#[test]
fn not_on_int_diagnoses() {
    let failure = typecheck_fail("fn main\n  not 1\nend\n");
    assert_eq!(failure.diagnostics.len(), 1);
    assert!(
        failure.diagnostics[0].message.contains("Bool operand"),
        "unexpected diagnostic: {}",
        failure.diagnostics[0].message,
    );
}

#[test]
fn neg_on_bool_diagnoses() {
    let failure = typecheck_fail("fn main\n  -true\nend\n");
    assert_eq!(failure.diagnostics.len(), 1);
    assert!(
        failure.diagnostics[0].message.contains("Int or Float"),
        "unexpected diagnostic: {}",
        failure.diagnostics[0].message,
    );
}

#[test]
fn string_concat_resolves_to_string() {
    assert_trailing_is("fn main\n  \"foo\" <> \"bar\"\nend\n", "String");
}

#[test]
fn binary_concat_requires_binary_operands() {
    // Cross-type concat (String <> Binary) is rejected: the user
    // must convert through an explicit stdlib helper. Pin the
    // diagnostic shape so accidental cross-type acceptance gets
    // caught.
    let failure = typecheck_fail(
        "fn copy(move b: Binary) -> Binary\n  \"hi\" <> b\nend\n\nfn main\n  1\nend\n",
    );
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("String, Binary, or Bits")),
        "expected concat-mismatch diagnostic; got {:?}",
        failure.diagnostics,
    );
}

#[test]
fn int_concat_diagnoses() {
    let failure = typecheck_fail("fn main\n  1 <> 2\nend\n");
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("String, Binary, or Bits")),
        "expected non-concat-typed diagnostic; got {:?}",
        failure.diagnostics,
    );
}

#[test]
fn float_arithmetic_resolves_to_float() {
    for source in [
        "fn main\n  1.0 + 2.0\nend\n",
        "fn main\n  1.0 - 2.0\nend\n",
        "fn main\n  1.0 * 2.0\nend\n",
        "fn main\n  1.0 / 2.0\nend\n",
        "fn main\n  1.0 % 2.0\nend\n",
    ] {
        let checked = typecheck(source);
        assert_eq!(
            trailing_resolution(&checked),
            float_type(&checked),
            "source = {source:?}",
        );
    }
}

#[test]
fn float_comparison_resolves_to_bool() {
    for source in [
        "fn main\n  1.0 < 2.0\nend\n",
        "fn main\n  1.0 > 2.0\nend\n",
        "fn main\n  1.0 <= 2.0\nend\n",
        "fn main\n  1.0 >= 2.0\nend\n",
    ] {
        let checked = typecheck(source);
        assert_eq!(
            trailing_resolution(&checked),
            bool_type(&checked),
            "source = {source:?}",
        );
    }
}

#[test]
fn float_equality_resolves_to_bool() {
    let checked = typecheck("fn main\n  1.0 == 2.0\nend\n");
    assert_eq!(trailing_resolution(&checked), bool_type(&checked));
}

#[test]
fn unary_neg_on_float_resolves_to_float() {
    let checked = typecheck("fn main\n  -3.14\nend\n");
    assert_eq!(trailing_resolution(&checked), float_type(&checked));
}

#[test]
fn mixed_int_float_arith_diagnoses() {
    let failure = typecheck_fail("fn main\n  1 + 1.0\nend\n");
    assert_eq!(failure.diagnostics.len(), 1);
    assert!(
        failure.diagnostics[0].message.contains("Int or Float"),
        "unexpected diagnostic: {}",
        failure.diagnostics[0].message,
    );
}

// ------------------------------------------------------------------
// `Int ≡ Int64` / `Float ≡ Float64` aliases at binary-op sites.
// Today the registry keeps these as distinct primitives papered over
// by `types_equivalent` (LANGUAGE.md primitives table); future Expo
// will promote `Int` to a real union over its sized variants. Both
// arithmetic and comparison must accept the alias mix today so FFI
// signatures returning `Int64` (or anything user-spelled as `Int64`)
// flow through naturally without forcing the caller to qualify.
// ------------------------------------------------------------------

fn use_alias_int(extra: &str) -> String {
    let extern_decl = "@extern \"C\"\nfn produce_int64() -> Int64\n\n";
    format!("{extern_decl}fn main\n  result = produce_int64()\n  {extra}\nend\n")
}

fn use_alias_float(extra: &str) -> String {
    let extern_decl = "@extern \"C\"\nfn produce_float64() -> Float64\n\n";
    format!("{extern_decl}fn main\n  result = produce_float64()\n  {extra}\nend\n")
}

#[test]
fn int_alias_arith_resolves_to_int() {
    let checked = typecheck(&use_alias_int("result + 1"));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn int_alias_comparison_resolves_to_bool() {
    for source in [
        use_alias_int("result == 0"),
        use_alias_int("result != 0"),
        use_alias_int("result < 0"),
        use_alias_int("result >= 0"),
    ] {
        let checked = typecheck(&source);
        assert_eq!(
            trailing_resolution(&checked),
            bool_type(&checked),
            "source = {source:?}",
        );
    }
}

#[test]
fn float_alias_arith_resolves_to_float() {
    let checked = typecheck(&use_alias_float("result + 1.0"));
    assert_eq!(trailing_resolution(&checked), float_type(&checked));
}

#[test]
fn float_alias_comparison_resolves_to_bool() {
    let checked = typecheck(&use_alias_float("result >= 0.0"));
    assert_eq!(trailing_resolution(&checked), bool_type(&checked));
}
