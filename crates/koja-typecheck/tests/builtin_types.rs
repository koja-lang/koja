//! Typecheck coverage for `builtin` type declarations: the stdlib's
//! doc anchors for compiler-owned types. Happy paths run implicitly
//! in every test through the autoimported stdlib, so this file pins
//! the claim's observable effect (builtin registry entries carry
//! stdlib source spans) plus the misuse diagnostics.

use koja_ast::identifier::Identifier;
use koja_ast::span::Span;
use koja_parser::ParseMode;

mod common;

use common::{assert_script_fails_with, check_packages, diagnostic_messages, typecheck_script};

/// Every seeded builtin stub, mirrored from
/// `GlobalRegistry::with_stdlib_stubs`.
const BUILTINS: &[&str] = &[
    "Int", "Bool", "Unit", "Float", "Never", "String", "Binary", "Bits", "Int8", "Int16", "Int32",
    "Int64", "UInt8", "UInt16", "UInt32", "UInt64", "Float32", "Float64", "CPtr", "List", "Map",
    "Set",
];

#[test]
fn stdlib_declarations_claim_every_builtin_stub() {
    let checked = typecheck_script("x = 1");
    for name in BUILTINS {
        let identifier = Identifier::new("Global", vec![(*name).to_string()]);
        let (_, entry) = checked
            .registry
            .lookup(&identifier)
            .unwrap_or_else(|| panic!("`{identifier}` missing from registry"));
        assert_ne!(
            entry.span,
            Span::default(),
            "`{identifier}` still carries the seeded stub span; expected the stdlib's \
             `builtin` declaration to claim it",
        );
    }
}

#[test]
fn builtin_outside_stdlib_diagnoses_not_builtin() {
    assert_script_fails_with(
        "
        builtin Config
        end
        ",
        &["`TestApp.Config` is not a builtin type"],
    );
}

#[test]
fn builtin_redeclaration_diagnoses_already_defined() {
    let failure = check_packages(
        &[(
            "Global",
            "dup.koja",
            "
            builtin String
            end
            ",
        )],
        ParseMode::File,
    )
    .expect_err("redeclaring a claimed builtin must fail");
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`Global.String` is already defined")),
        "expected an already-defined diagnostic, got: {messages:#?}",
    );
}

#[test]
fn private_builtin_diagnoses() {
    assert_script_fails_with(
        "
        priv builtin Shadow
        end
        ",
        &["builtin type `Shadow` cannot be private"],
    );
}

#[test]
fn builtin_construction_diagnoses() {
    assert_script_fails_with(
        "s = String{}",
        &["cannot construct builtin type `Global.String` with struct literal syntax"],
    );
}

#[test]
fn intrinsic_on_struct_diagnoses_replacement() {
    assert_script_fails_with(
        "
        @intrinsic
        struct Config
        end
        ",
        &["`@intrinsic` on struct `Config` is replaced by the `builtin` declaration"],
    );
}

#[test]
fn intrinsic_on_enum_diagnoses() {
    assert_script_fails_with(
        "
        @intrinsic
        enum Direction
          North
          South
        end
        ",
        &["typecheck does not yet support annotations on enum items (`@intrinsic` on `Direction`)"],
    );
}
