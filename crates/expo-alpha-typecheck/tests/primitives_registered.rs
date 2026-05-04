//! Coverage that `check_program` preloads the stdlib struct stubs
//! (`Int`, `Bool`, `Unit`, `Float`, `String`) into the
//! [`GlobalRegistry`] before user decls are collected.
//!
//! These stubs are temporary scaffolding -- once the real stdlib is
//! compiled as a package they land through `collect` like any other
//! decl. The test still matters post-cutover: the exact same
//! assertions should pass against the real entries because the stubs
//! and the real stdlib share the same shape (`Global.<name>` struct
//! in the registry).

use std::path::PathBuf;

use expo_alpha_typecheck::{CheckedProgram, GlobalKind, check_program};
use expo_ast::identifier::Identifier;
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

const STDLIB_STUBS: &[&str] = &["Int", "Bool", "Unit", "Float", "String"];

fn check_empty_main() -> CheckedProgram {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("primitives_registered.expo"),
            source: "fn main\n  1\nend\n".to_string(),
        }],
        ParseMode::File,
    );
    check_program(parsed).unwrap_or_else(|failure| {
        panic!(
            "alpha typecheck failed on minimal program: {} diagnostic(s):\n{failure}",
            failure.diagnostics.len()
        )
    })
}

#[test]
fn stdlib_stubs_land_in_registry_as_structs() {
    let checked = check_empty_main();

    for name in STDLIB_STUBS {
        let ident = Identifier::new("Global", vec![(*name).to_string()]);
        let (id, entry) = checked.registry.lookup(&ident).unwrap_or_else(|| {
            panic!("stdlib stub `Global.{name}` missing from registry after check_program")
        });

        assert_eq!(
            entry.kind,
            GlobalKind::Struct,
            "Global.{name} registered with wrong kind: {:?}",
            entry.kind,
        );
        assert_eq!(
            entry.identifier, ident,
            "Global.{name}'s entry identifier drifted: {}",
            entry.identifier,
        );
        assert!(
            entry.identifier.is_in_global(),
            "Global.{name}'s entry is not in the Global package",
        );

        // Forward lookup by id should round-trip back to the same entry.
        let round_trip = checked
            .registry
            .get(id)
            .expect("registry.get on a freshly-returned id must succeed");
        assert_eq!(round_trip.identifier, ident);
    }
}

#[test]
fn stdlib_stubs_precede_user_decls() {
    let checked = check_empty_main();

    // User decl: TestApp.main. Must exist and must have been assigned
    // a strictly-greater id than every stdlib stub, since the preload
    // runs before `collect`.
    let main_ident = Identifier::new(PACKAGE, vec!["main".to_string()]);
    let (main_id, _) = checked
        .registry
        .lookup(&main_ident)
        .expect("TestApp.main missing from registry");

    for name in STDLIB_STUBS {
        let ident = Identifier::new("Global", vec![(*name).to_string()]);
        let (stub_id, _) = checked.registry.lookup(&ident).expect("stub missing");
        assert!(
            stub_id < main_id,
            "stdlib stub Global.{name} ({stub_id}) should precede user decl {main_ident} ({main_id})",
        );
    }
}
