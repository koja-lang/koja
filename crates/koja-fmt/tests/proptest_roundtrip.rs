//! Property-based tests for the formatter.
//!
//! - `corpus_idempotence`: every `.koja` and `.kojs` file under `lib/` and
//!   `tests/lang/` (excluding `compile_fail/`) must format successfully and
//!   reach a fixed point on the second formatting pass. `.koja` files are
//!   formatted in [`ParseMode::File`] and `.kojs` scripts in
//!   [`ParseMode::Script`], dispatched via [`ParseMode::for_path`].
//! - `corpus_canonical`: every `.koja` file under `lib/` (the standard
//!   library) must already be in canonical form: `format(src) == src`
//!   byte-for-byte. Test fixtures under `tests/lang/` are not held to this
//!   bar and may intentionally exercise non-canonical input.
//! - `corpus_comments_preserved`: formatting never loses or invents a
//!   comment. The multiset of trimmed comment texts must match between
//!   input and output for every corpus file.
//! - The `proptest!` block exercises the formatter with random inputs,
//!   asserting that it never panics and that any successfully-formatted
//!   output is itself parseable and idempotent, and fuzzes corpus files
//!   with injected comments to assert preservation.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use koja_fmt::{FormatResult, format};
use koja_parser::ParseMode;
use proptest::prelude::*;

/// Collects source files under `roots` whose extension is in `extensions`,
/// skipping `compile_fail/` directories. Results are sorted for determinism.
fn collect_files(roots: &[&Path], extensions: &[&str]) -> Vec<PathBuf> {
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
            } else if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| extensions.contains(&ext))
            {
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

fn fmt_ok(src: &str, mode: ParseMode) -> Option<String> {
    match format(src, mode) {
        FormatResult::Ok(s) => Some(s),
        FormatResult::ParseErrors(_) => None,
    }
}

/// Parses `src` and returns the multiset of trimmed comment texts, or
/// `None` on parse errors. Trimmed because the formatter normalizes
/// comment whitespace to `# text`.
fn comment_multiset(src: &str, mode: ParseMode) -> Option<BTreeMap<String, usize>> {
    let result = koja_parser::parse(src, mode);
    if !result.errors.is_empty() {
        return None;
    }
    let mut counts = BTreeMap::new();
    for comment in &result.ast.comments {
        *counts.entry(comment.text.trim().to_string()).or_insert(0) += 1;
    }
    Some(counts)
}

#[test]
fn corpus_idempotence() {
    let lib = lib_root();
    let tests_lang = tests_lang_root();
    let roots = [lib.as_path(), tests_lang.as_path()];
    let fixtures = collect_files(&roots, &["koja", "kojs"]);
    assert!(!fixtures.is_empty(), "no fixtures found");

    let mut failures = Vec::new();
    for path in &fixtures {
        let mode = ParseMode::for_path(path);
        let src = match fs::read_to_string(path) {
            Ok(src) => src,
            Err(err) => {
                failures.push(format!("{}: read error: {err}", path.display()));
                continue;
            }
        };
        let Some(once) = fmt_ok(&src, mode) else {
            failures.push(format!("{}: failed to parse/format", path.display()));
            continue;
        };
        let Some(twice) = fmt_ok(&once, mode) else {
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
    let fixtures = collect_files(&[root.as_path()], &["koja"]);
    assert!(
        !fixtures.is_empty(),
        "no stdlib files found under {}",
        root.display()
    );

    let mut failures = Vec::new();
    for path in &fixtures {
        let src = fs::read_to_string(path).unwrap();
        let Some(formatted) = fmt_ok(&src, ParseMode::File) else {
            failures.push(format!("{}: failed to parse/format", path.display()));
            continue;
        };
        if src != formatted {
            failures.push(format!(
                "{}: not in canonical form (run `koja format`)",
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

#[test]
fn corpus_comments_preserved() {
    let lib = lib_root();
    let tests_lang = tests_lang_root();
    let roots = [lib.as_path(), tests_lang.as_path()];
    let fixtures = collect_files(&roots, &["koja", "kojs"]);
    assert!(!fixtures.is_empty(), "no fixtures found");

    let mut failures = Vec::new();
    for path in &fixtures {
        let mode = ParseMode::for_path(path);
        let src = fs::read_to_string(path).unwrap();
        let Some(before) = comment_multiset(&src, mode) else {
            failures.push(format!("{}: failed to parse", path.display()));
            continue;
        };
        let Some(formatted) = fmt_ok(&src, mode) else {
            failures.push(format!("{}: failed to format", path.display()));
            continue;
        };
        let Some(after) = comment_multiset(&formatted, mode) else {
            failures.push(format!(
                "{}: formatted output failed to reparse",
                path.display()
            ));
            continue;
        };
        if before != after {
            failures.push(format!(
                "{}: comments changed\n--- before ---\n{before:?}\n--- after ---\n{after:?}",
                path.display()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} fixture(s) lost or invented comments:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// The corpus loaded once for comment-injection fuzzing.
fn injection_corpus() -> &'static Vec<(ParseMode, String)> {
    static CORPUS: OnceLock<Vec<(ParseMode, String)>> = OnceLock::new();
    CORPUS.get_or_init(|| {
        let lib = lib_root();
        let tests_lang = tests_lang_root();
        let roots = [lib.as_path(), tests_lang.as_path()];
        collect_files(&roots, &["koja", "kojs"])
            .into_iter()
            .filter_map(|path| {
                let mode = ParseMode::for_path(&path);
                fs::read_to_string(&path).ok().map(|src| (mode, src))
            })
            .collect()
    })
}

/// One comment to inject into a corpus file: a line index selector, a
/// standalone-vs-trailing flag, and the comment text.
type Injection = (prop::sample::Index, bool, String);

/// Injects comments into `src`. Standalone injections take their own line
/// above the selected line, and trailing injections append to it.
fn inject_comments(src: &str, injections: &[Injection]) -> (String, Vec<String>) {
    let mut lines: Vec<String> = src.lines().map(str::to_string).collect();
    if lines.is_empty() {
        lines.push(String::new());
    }
    let mut injected_texts = Vec::new();
    for (index, standalone, text) in injections {
        let line_idx = index.index(lines.len());
        if *standalone {
            lines.insert(line_idx, format!("# {text}"));
        } else {
            lines[line_idx] = format!("{} # {text}", lines[line_idx]);
        }
        injected_texts.push(text.clone());
    }
    (lines.join("\n") + "\n", injected_texts)
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
        let Some(once) = fmt_ok(&s, ParseMode::File) else { return Ok(()); };
        let Some(twice) = fmt_ok(&once, ParseMode::File) else {
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

    /// Injects random comments into corpus files and asserts the formatter
    /// preserves every one and stays idempotent. Injections that land
    /// inside strings or break the parse are discarded via the multiset
    /// pre-check, so surviving cases are genuine comments.
    #[test]
    fn injected_comments_survive_formatting(
        file_index in any::<prop::sample::Index>(),
        injections in prop::collection::vec(
            (any::<prop::sample::Index>(), any::<bool>(), "[a-z][a-z0-9 ]{0,15}"),
            1..6,
        ),
    ) {
        let corpus = injection_corpus();
        prop_assume!(!corpus.is_empty());
        let (mode, src) = &corpus[file_index.index(corpus.len())];

        let Some(mut expected) = comment_multiset(src, *mode) else {
            return Err(TestCaseError::fail("corpus file failed to parse"));
        };
        let (injected_src, injected_texts) = inject_comments(src, &injections);
        for text in &injected_texts {
            *expected.entry(text.trim().to_string()).or_insert(0) += 1;
        }

        // Discard cases where an injection broke the parse or was swallowed
        // by a string literal instead of lexing as a comment.
        let Some(parsed) = comment_multiset(&injected_src, *mode) else {
            return Ok(());
        };
        prop_assume!(parsed == expected);

        let Some(once) = fmt_ok(&injected_src, *mode) else {
            return Err(TestCaseError::fail(format!(
                "injected source parsed but failed to format:\n{injected_src}"
            )));
        };
        let after = comment_multiset(&once, *mode).ok_or_else(|| {
            TestCaseError::fail(format!("formatted output failed to reparse:\n{once}"))
        })?;
        prop_assert_eq!(&after, &expected, "comments lost or invented:\n{}", once);

        let Some(twice) = fmt_ok(&once, *mode) else {
            return Err(TestCaseError::fail(format!(
                "second pass failed to reparse:\n{once}"
            )));
        };
        prop_assert_eq!(once, twice, "not idempotent after comment injection");
    }
}
