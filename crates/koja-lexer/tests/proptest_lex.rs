//! Property-based tests for the lexer.
//!
//! - `corpus_lexes_clean`: every `.koja` file under `lib/` and `tests/lang/`
//!   (excluding `compile_fail/`) lexes with no errors.
//! - The `proptest!` block exercises the lexer with random inputs, asserting
//!   that it never panics, produces well-formed spans (in-bounds, on UTF-8
//!   character boundaries), is always EOF-terminated, and is deterministic.
//!
//! The fixture walker is duplicated across the layer-1 proptest files
//! (lex/parse/fmt) intentionally. Once we add a fourth site this should
//! move to a shared `koja-test-support` dev-only crate.

use std::fs;
use std::path::{Path, PathBuf};

use koja_lexer::{Span, TokenKind, lex};
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

/// Checks that span offsets are ordered UTF-8 boundaries within the source.
fn span_well_formed(span: &Span, source: &str) -> Result<(), String> {
    let start = span.start.offset as usize;
    let end = span.end.offset as usize;
    if start > end {
        return Err(format!("span start ({start}) > end ({end})"));
    }
    if end > source.len() {
        return Err(format!(
            "span end ({end}) exceeds source length ({})",
            source.len()
        ));
    }
    if !source.is_char_boundary(start) {
        return Err(format!("span start ({start}) is not a UTF-8 boundary"));
    }
    if !source.is_char_boundary(end) {
        return Err(format!("span end ({end}) is not a UTF-8 boundary"));
    }
    Ok(())
}

#[test]
fn corpus_lexes_clean() {
    let roots = [lib_root(), tests_lang_root()];
    let root_refs: Vec<&Path> = roots.iter().map(|p| p.as_path()).collect();
    let fixtures = collect_koja_files(&root_refs);
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
        let result = lex(&src);
        if !result.errors.is_empty() {
            failures.push(format!(
                "{}: {} lex error(s): {:?}",
                path.display(),
                result.errors.len(),
                result.errors
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} fixture(s) failed clean-lex:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

proptest! {
    #[test]
    fn never_panics_on_random_string(s in ".{0,500}") {
        let _ = lex(&s);
    }

    #[test]
    fn never_panics_on_random_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..500)) {
        let s = String::from_utf8_lossy(&bytes);
        let _ = lex(&s);
    }

    #[test]
    fn spans_are_well_formed(s in ".{0,500}") {
        let result = lex(&s);
        for token in &result.tokens {
            if let Err(err) = span_well_formed(&token.span, &s) {
                return Err(TestCaseError::fail(format!(
                    "ill-formed token span: {err}\nsource: {s:?}\ntoken: {token:?}"
                )));
            }
        }
        for diag in &result.errors {
            if let Err(err) = span_well_formed(&diag.span, &s) {
                return Err(TestCaseError::fail(format!(
                    "ill-formed diagnostic span: {err}\nsource: {s:?}\ndiag: {diag:?}"
                )));
            }
        }
    }

    #[test]
    fn always_terminated_by_eof(s in ".{0,500}") {
        let result = lex(&s);
        let last = result.tokens.last();
        prop_assert!(
            matches!(last.map(|t| &t.kind), Some(TokenKind::EndOfFile)),
            "token stream not terminated by EOF: last = {last:?}"
        );
    }

    #[test]
    fn deterministic(s in ".{0,500}") {
        let a = lex(&s);
        let b = lex(&s);
        prop_assert_eq!(a.tokens, b.tokens);
        prop_assert_eq!(a.errors.len(), b.errors.len());
    }
}

#[test]
fn lexer_offset_should_be_byte_indexed() {
    for source in ["¡", "a¡b", "😀"] {
        let result = lex(source);
        let eof = result.tokens.last().expect("at least one token");
        assert_eq!(eof.span.start.offset, source.len() as u32);
        assert_eq!(eof.span.end.offset, source.len() as u32);
    }
}
