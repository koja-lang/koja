//! Typecheck coverage for `@intrinsic` type declarations: the
//! stdlib's doc anchors for compiler-defined builtins. Happy paths
//! run implicitly in every test through the autoimported stdlib, so
//! this file pins the claim's observable effect (builtin registry
//! entries carry stdlib source spans) plus the misuse diagnostics.

use koja_ast::identifier::Identifier;
use koja_ast::span::Span;
use koja_parser::ParseMode;

mod common;

use common::{assert_script_fails_with, check_packages, diagnostic_messages, typecheck_script};

/// Every seeded primitive stub, mirrored from
/// `GlobalRegistry::with_stdlib_stubs`.
const BUILTINS: &[&str] = &[
    "Int", "Bool", "Unit", "Float", "Never", "String", "Binary", "Bits", "Int8", "Int16", "Int32",
    "Int64", "UInt8", "UInt16", "UInt32", "UInt64", "Float32", "Float64",
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
             `@intrinsic` declaration to claim it",
        );
    }
}

#[test]
fn intrinsic_struct_outside_stdlib_diagnoses_not_builtin() {
    assert_script_fails_with(
        "
        @intrinsic
        struct Config
        end
        ",
        &["`TestApp.Config` is not a builtin type"],
    );
}

#[test]
fn intrinsic_redeclaration_of_builtin_diagnoses_already_defined() {
    let failure = check_packages(
        &[(
            "Global",
            "dup.koja",
            "
            @intrinsic
            struct String
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
fn private_intrinsic_struct_diagnoses() {
    assert_script_fails_with(
        "
        @intrinsic
        priv struct Shadow
        end
        ",
        &["`@intrinsic` struct `Shadow` cannot be private"],
    );
}

#[test]
fn generic_intrinsic_struct_diagnoses() {
    assert_script_fails_with(
        "
        @intrinsic
        struct Wrapper<T>
        end
        ",
        &["`@intrinsic` struct `Wrapper` cannot declare type parameters"],
    );
}

#[test]
fn intrinsic_struct_with_fields_diagnoses() {
    assert_script_fails_with(
        "
        @intrinsic
        struct Sized
          width: Int
        end
        ",
        &["`@intrinsic` struct `Sized` cannot declare fields"],
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
