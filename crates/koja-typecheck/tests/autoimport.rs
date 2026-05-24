//! Smoke test that every entry in [`koja_stdlib::AUTOIMPORT`]
//! typechecks cleanly when prepended to a trivial user file.
//! Failure here is the canary that a stdlib file's surface drifted
//! from what the typechecker supports.

use std::path::PathBuf;

use koja_parser::{ParseMode, SourceFile, parse_program};
use koja_typecheck::check_program;

#[test]
fn autoimport_typechecks_with_user_main() {
    let mut sources = koja_stdlib::autoimport_sources();
    sources.push(SourceFile {
        package: "TestApp".to_string(),
        path: PathBuf::from("main.koja"),
        source: "fn main\n  1\nend\n".to_string(),
    });

    let parsed = parse_program(sources, ParseMode::File);
    if let Err(failure) = check_program(parsed) {
        let messages: Vec<String> = failure
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect();
        panic!(
            "auto-import failed to typecheck ({} diagnostic(s)):\n{}",
            messages.len(),
            messages.join("\n"),
        );
    }
}
