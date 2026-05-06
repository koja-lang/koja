//! Coverage that `check_program` preloads the stdlib struct stubs
//! (`Int`/`Bool`/`Unit`/`Float`/`String`) into the [`GlobalRegistry`]
//! before user decls are collected.
//!
//! Stubs are temporary scaffolding — once the real stdlib compiles as
//! a package they land through `collect`. These assertions stay valid
//! post-cutover because stubs and real entries share the same shape
//! (`Global.<name>` struct in the registry).

use expo_alpha_typecheck::{CheckedProgram, GlobalKind};
use expo_ast::identifier::Identifier;

mod common;

use common::{PACKAGE, typecheck_file};

const STDLIB_STUBS: &[&str] = &["Int", "Bool", "Unit", "Float", "String"];

fn check_empty_main() -> CheckedProgram {
    typecheck_file("fn main\n  1\nend\n")
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
            GlobalKind::Struct(None),
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
