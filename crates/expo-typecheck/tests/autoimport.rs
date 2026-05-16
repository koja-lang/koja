//! Smoke test that every entry in [`expo_stdlib::AUTOIMPORT`]
//! typechecks cleanly when prepended to a trivial user file.
//! Failure here is the canary that a stdlib file's surface drifted
//! from what the typechecker supports.

use std::path::PathBuf;

use expo_parser::{ParseMode, SourceFile, parse_program};
use expo_typecheck::check_program;

#[test]
fn autoimport_typechecks_with_user_main() {
    let mut sources = expo_stdlib::autoimport_sources();
    sources.push(SourceFile {
        package: "TestApp".to_string(),
        path: PathBuf::from("main.expo"),
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
