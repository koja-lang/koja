//! Property-based tests for the parser.
//!
//! - `corpus_parses_clean`: every `.expo` file under `lib/` and `tests/lang/`
//!   (excluding `compile_fail/`) parses with no errors.
//! - The `proptest!` block exercises the parser with random inputs, asserting
//!   that it never panics, that diagnostic spans are well-formed, that
//!   diagnostic messages are non-empty, and that parsing is deterministic.
//!
//! The fixture walker is duplicated across the layer-1 proptest files
//! (lex/parse/fmt) intentionally — once we add a fourth site this should
//! move to a shared `expo-test-support` dev-only crate.

use std::fs;
use std::path::{Path, PathBuf};

use expo_parser::parse;
use proptest::prelude::*;

fn collect_expo_files(roots: &[&Path]) -> Vec<PathBuf> {
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
            } else if path.extension().is_some_and(|ext| ext == "expo") {
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

/// Checks the span invariants the parser currently upholds: `start <= end`
/// and `end` does not exceed the source length.
///
/// The stricter "offset lands on a UTF-8 char boundary" property is not
/// asserted here because the underlying lexer cursor is char-indexed, not
/// byte-indexed (see `lexer_offset_should_be_byte_indexed` in the
/// `expo-lexer` proptest suite).
fn span_within_source(start: u32, end: u32, source_len: usize) -> Result<(), String> {
    if start > end {
        return Err(format!("span start ({start}) > end ({end})"));
    }
    if end as usize > source_len {
        return Err(format!(
            "span end ({end}) exceeds source length ({source_len})"
        ));
    }
    Ok(())
}

#[test]
fn corpus_parses_clean() {
    let roots = [lib_root(), tests_lang_root()];
    let root_refs: Vec<&Path> = roots.iter().map(|p| p.as_path()).collect();
    let fixtures = collect_expo_files(&root_refs);
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
        let result = parse(&src);
        if !result.errors.is_empty() {
            failures.push(format!(
                "{}: {} parse error(s):\n{}",
                path.display(),
                result.errors.len(),
                result
                    .errors
                    .iter()
                    .map(|e| format!("  - {}", e.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} fixture(s) failed clean-parse:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

proptest! {
    #[test]
    fn never_panics_on_random_string(s in ".{0,500}") {
        let _ = parse(&s);
    }

    #[test]
    fn never_panics_on_random_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..500)) {
        let s = String::from_utf8_lossy(&bytes);
        let _ = parse(&s);
    }

    #[test]
    fn diagnostic_spans_well_formed(s in ".{0,500}") {
        let result = parse(&s);
        for diag in &result.errors {
            if let Err(err) = span_within_source(
                diag.span.start.offset,
                diag.span.end.offset,
                s.len(),
            ) {
                return Err(TestCaseError::fail(format!(
                    "ill-formed diagnostic span: {err}\nsource: {s:?}\ndiag: {diag:?}"
                )));
            }
        }
    }

    #[test]
    fn diagnostic_messages_non_empty(s in ".{0,500}") {
        let result = parse(&s);
        for diag in &result.errors {
            prop_assert!(
                !diag.message.is_empty(),
                "empty diagnostic message: {diag:?}"
            );
        }
    }
}

#[test]
fn deterministic_on_corpus() {
    let roots = [lib_root(), tests_lang_root()];
    let root_refs: Vec<&Path> = roots.iter().map(|p| p.as_path()).collect();
    let fixtures = collect_expo_files(&root_refs);

    for path in &fixtures {
        let src = fs::read_to_string(path).unwrap();
        let a = parse(&src);
        let b = parse(&src);
        assert_eq!(
            a.errors.len(),
            b.errors.len(),
            "non-deterministic error count for {}",
            path.display()
        );
        for (ea, eb) in a.errors.iter().zip(b.errors.iter()) {
            assert_eq!(
                ea.message,
                eb.message,
                "non-deterministic error message for {}",
                path.display()
            );
        }
    }
}
