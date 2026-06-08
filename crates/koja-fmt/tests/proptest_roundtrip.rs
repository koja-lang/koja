//! Property-based tests for the formatter.
//!
//! - `corpus_idempotence`: every `.koja` file under `lib/` and `tests/lang/`
//!   (excluding `compile_fail/`) must format successfully and reach a fixed
//!   point on the second formatting pass.
//! - `corpus_canonical`: every `.koja` file under `lib/` (the standard
//!   library) must already be in canonical form: `format(src) == src`
//!   byte-for-byte. Test fixtures under `tests/lang/` are not held to this
//!   bar — they may intentionally exercise non-canonical input.
//! - The `proptest!` block exercises the formatter with random inputs,
//!   asserting that it never panics and that any successfully-formatted
//!   output is itself parseable and idempotent.

use std::fs;
use std::path::{Path, PathBuf};

use koja_fmt::{FormatResult, format};
use koja_parser::ParseMode;
use proptest::prelude::*;

fn collect_koja_files(roots: &[&Path]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = roots.iter().map(|r| r.to_path_buf()).collect();
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "compile_fail") {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "koja") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn lib_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib")
}

fn tests_lang_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/lang")
}

fn fmt_ok(src: &str) -> Option<String> {
    match format(src, ParseMode::File) {
        FormatResult::Ok(s) => Some(s),
        FormatResult::ParseErrors(_) => None,
    }
}

#[test]
fn corpus_idempotence() {
    let lib = lib_root();
    let tests_lang = tests_lang_root();
    let roots = [lib.as_path(), tests_lang.as_path()];
    let fixtures = collect_koja_files(&roots);
    assert!(!fixtures.is_empty(), "no fixtures found");

    let mut failures = Vec::new();
    for path in &fixtures {
        let src = match fs::read_to_string(path) {
            Ok(src) => src,
            Err(err) => {
                failures.push(format!("{}: read error: {err}", path.display()));
                continue;
            }
        };
        let Some(once) = fmt_ok(&src) else {
            failures.push(format!("{}: failed to parse/format", path.display()));
            continue;
        };
        let Some(twice) = fmt_ok(&once) else {
            failures.push(format!(
                "{}: formatted output failed to reparse",
                path.display()
            ));
            continue;
        };
        if once != twice {
            failures.push(format!(
                "{}: not idempotent\n--- once ---\n{once}--- twice ---\n{twice}",
                path.display()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} fixture(s) failed idempotence:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn corpus_canonical() {
    let root = lib_root();
    let fixtures = collect_koja_files(&[root.as_path()]);
    assert!(
        !fixtures.is_empty(),
        "no stdlib files found under {}",
        root.display()
    );

    let mut failures = Vec::new();
    for path in &fixtures {
        let src = fs::read_to_string(path).unwrap();
        let Some(formatted) = fmt_ok(&src) else {
            failures.push(format!("{}: failed to parse/format", path.display()));
            continue;
        };
        if src != formatted {
            failures.push(format!(
                "{}: not in canonical form (run `koja format --write`)",
                path.display()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} stdlib file(s) drifted from canonical form:\n\n{}",
        failures.len(),
        failures.join("\n")
    );
}

proptest! {
    #[test]
    fn never_panics_on_random_string(s in ".{0,500}") {
        let _ = format(&s, ParseMode::File);
    }

    #[test]
    fn never_panics_on_random_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..500)) {
        let s = String::from_utf8_lossy(&bytes);
        let _ = format(&s, ParseMode::File);
    }

    #[test]
    fn idempotent_on_parseable_random(s in ".{0,500}") {
        let Some(once) = fmt_ok(&s) else { return Ok(()); };
        let Some(twice) = fmt_ok(&once) else {
            return Err(TestCaseError::fail(format!(
                "formatted output failed to reparse:\n{once}"
            )));
        };
        prop_assert_eq!(once, twice);
    }

    #[test]
    fn formatted_output_always_parses(s in ".{0,500}") {
        if let FormatResult::Ok(out) = format(&s, ParseMode::File) {
            let result = koja_parser::parse(&out, ParseMode::File);
            prop_assert!(
                result.errors.is_empty(),
                "formatter produced un-parseable output:\n{out}"
            );
        }
    }
}
