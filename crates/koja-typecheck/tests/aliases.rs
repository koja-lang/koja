//! `alias Pkg.Type [as Local]` validation + use-site resolution.
//!
//! Pins the contract surface for [`koja_typecheck::pipeline::aliases`]:
//! every alias must qualify (`Package.Type`), point at a registered
//! struct/enum/protocol, name-collide with no other alias in the
//! file, and *not* shadow a current-package or `Global` binding
//! (carve-out: redundant self-aliases pointing at the very same
//! identifier they would shadow are allowed). Use sites resolve via
//! the rewritten alias target and never fall through to the
//! "unknown type" diagnostic when the alias is well-formed.

use koja_parser::ParseMode;

mod common;

use common::{
    assert_file_fails_with, check_multi_file, diagnostic_messages, typecheck_file,
    typecheck_file_fail,
};

#[test]
fn alias_to_global_struct_resolves_locally() {
    // `Global.List` is reachable as bare `List` everywhere via the
    // primitive lookup fallthrough. Aliasing it under its real name
    // should still work (it's a no-op alias-wise, but should not
    // diagnose since the alias target == the existing binding, the
    // redundant-self-alias carve-out applies).
    typecheck_file(
        "alias Global.List as MyList\n\
         fn make -> MyList<Int>\n  List.new()\nend\n",
    );
}

#[test]
fn alias_with_as_renames_locally() {
    let checked = typecheck_file(
        "alias Crypto.SHA256 as Hasher\n\
         fn run(data: Binary) -> Binary\n  Hasher.digest(data)\nend\n",
    );
    assert!(
        checked.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        checked.diagnostics,
    );
}

#[test]
fn alias_default_local_name() {
    let checked = typecheck_file(
        "alias Crypto.SHA256\n\
         fn run(data: Binary) -> Binary\n  SHA256.digest(data)\nend\n",
    );
    assert!(
        checked.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        checked.diagnostics,
    );
}

#[test]
fn alias_unknown_package_diagnoses() {
    assert_file_fails_with(
        "alias Nope.Thing as Thing\n\
         fn main\n  1\nend\n",
        &["alias target `Nope.Thing` is not a registered type"],
    );
}

#[test]
fn alias_unknown_type_diagnoses() {
    assert_file_fails_with(
        "alias Crypto.NoSuchType\n\
         fn main\n  1\nend\n",
        &["alias target `Crypto.NoSuchType` is not a registered type"],
    );
}

#[test]
fn alias_path_too_short_diagnoses() {
    assert_file_fails_with(
        "alias Foo\n\
         fn main\n  1\nend\n",
        &["alias path must be `Package.Type`"],
    );
}

#[test]
fn alias_multi_segment_target_falls_through_when_unregistered() {
    // Alias machinery is path-length agnostic: `alias Pkg.Outer.Inner`
    // validates structurally (path length >= 2 passes) but errors at
    // "type not registered" until nested-type lifting populates
    // multi-segment targets in the registry. Pinned so the alias
    // helper stays load-bearing for the eventual nested-type slice:
    // the helper resolves both `O` (1 segment) and `O.Inner` (2)
    // without code movement once the registry carries those entries.
    assert_file_fails_with(
        "alias Crypto.SHA256.Inner as Inner\n\
         fn main\n  1\nend\n",
        &["alias target `Crypto.SHA256.Inner` is not a registered type"],
    );
}

#[test]
fn alias_duplicate_local_name_diagnoses() {
    assert_file_fails_with(
        "alias Crypto.SHA256 as Hasher\n\
         alias Crypto.SHA1 as Hasher\n\
         fn main\n  1\nend\n",
        &["duplicate alias `Hasher`"],
    );
}

#[test]
fn alias_shadowing_global_is_error() {
    assert_file_fails_with(
        "alias Crypto.SHA256 as Int\n\
         fn main\n  1\nend\n",
        &["alias `Int` would shadow", "Global.Int"],
    );
}

#[test]
fn alias_shadowing_current_package_is_error() {
    assert_file_fails_with(
        "struct Foo\nend\n\
         alias Crypto.SHA256 as Foo\n\
         fn main\n  1\nend\n",
        &["alias `Foo` would shadow", "TestApp.Foo"],
    );
}

#[test]
fn alias_to_self_is_redundant_but_not_shadow() {
    // `alias TestApp.Foo as Foo` is redundant (the local name binds
    // the same identifier the current-package shadow check would
    // surface), but the carve-out in `validate_aliases` allows it.
    // No diagnostic should fire.
    let checked = typecheck_file(
        "struct Foo\nend\n\
         alias TestApp.Foo as Foo\n\
         fn main\n  Foo{}\nend\n",
    );
    assert!(
        checked.diagnostics.is_empty(),
        "expected no diagnostics for redundant self-alias, got {:?}",
        checked.diagnostics,
    );
}

#[test]
fn alias_is_file_private() {
    // `a.koja` defines an alias. `b.koja` (sister file in the same
    // package) tries to use the alias's local name. Aliases are
    // file-private, so `b.koja` should fail to resolve `Hasher`.
    let result = check_multi_file(
        &[
            (
                "a.koja",
                "alias Crypto.SHA256 as Hasher\n\
                 fn use_a(data: Binary) -> Binary\n  Hasher.digest(data)\nend\n",
            ),
            (
                "b.koja",
                "fn use_b(data: Binary) -> Binary\n  Hasher.digest(data)\nend\n",
            ),
        ],
        ParseMode::File,
    );
    let failure = result.expect_err("sister file should not see a.koja's alias");
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("Hasher") || m.contains("does not recognize")),
        "expected unknown-Hasher diagnostic in b.koja, got: {messages:?}",
    );
}

#[test]
fn dotted_static_call_resolves_without_alias() {
    // Bare dotted static dispatch: `Crypto.SHA256.digest(...)` with
    // no `alias` line. Pre-PR-B this hit the
    // "typecheck does not yet support dotted type names"
    // gate. Post-PR-B `classify_receiver` walks the FieldAccess
    // chain and `lookup_type` finds the qualified entry directly.
    let checked =
        typecheck_file("fn run(data: Binary) -> Binary\n  Crypto.SHA256.digest(data)\nend\n");
    assert!(
        checked.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        checked.diagnostics,
    );
}

#[test]
fn dotted_type_in_signature_resolves_without_alias() {
    // Dotted type in a parameter position: `crypto: Crypto.SHA256`
    // with no `alias` line. Same gate as the static call above:
    // `resolve_named` walks the path through `resolve_path_to_global`
    // and finds `Crypto.SHA256` directly. Body just borrows the
    // value, so the receiver is exercised purely as a type
    // annotation.
    let checked =
        typecheck_file("fn touch(crypto: Crypto.SHA256) -> Crypto.SHA256\n  crypto\nend\n");
    assert!(
        checked.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        checked.diagnostics,
    );
}

#[test]
fn dotted_static_call_unknown_path_diagnoses() {
    // Negative companion to the dotted static-call test above: a
    // path with no registry entry should fall through with a clean
    // "type not registered" diagnostic, *not* a feature-gap message
    // about dotted names. Pinned so removing the dotted gate
    // doesn't silently swallow real "you typo'd a package name"
    // errors.
    let failure = typecheck_file_fail("fn main\n  No.Such.Thing.foo()\nend\n");
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| !m.contains("does not yet support dotted type names")),
        "diagnostic must not regress to the feature-gap message: {messages:?}",
    );
    assert!(
        messages.iter().any(|m| m.contains("No")),
        "expected a diagnostic mentioning the path head `No`, got: {messages:?}",
    );
}

#[test]
fn type_param_shadows_alias_inside_function() {
    // File-level `alias Crypto.SHA256 as T` is well-formed: `T` is
    // not a current-package or `Global` binding. Inside a function
    // declaring its own `<T>`, the type parameter shadows the alias
    // by lexical scope (the lookup order in `resolve_named` checks
    // type params before file aliases). The function body must
    // typecheck cleanly with `T` resolving to the function's own
    // type parameter, not the aliased struct.
    let checked = typecheck_file(
        "alias Crypto.SHA256 as T\n\
         fn identity<T>(value: T) -> T\n  value\nend\n\
         fn main\n  identity(1)\nend\n",
    );
    assert!(
        checked.diagnostics.is_empty(),
        "expected no diagnostics for alias-vs-type-param scoping, got {:?}",
        checked.diagnostics,
    );
}
