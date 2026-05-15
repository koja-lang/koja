//! Smoke test that every entry in [`expo_stdlib::ALPHA_AUTOIMPORT`]
//! typechecks cleanly when prepended to a trivial user file.
//! Failure here is the canary that a stdlib file's surface drifted
//! from what the alpha typecheck supports.

use std::path::PathBuf;

use expo_alpha_typecheck::check_program;
use expo_parser::{ParseMode, SourceFile, parse_program};

#[test]
fn alpha_autoimport_typechecks_with_user_main() {
    let mut sources = expo_stdlib::alpha_autoimport_sources();
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
            "alpha auto-import failed to typecheck ({} diagnostic(s)):\n{}",
            messages.len(),
            messages.join("\n"),
        );
    }
}
