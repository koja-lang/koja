//! Coverage for the file-aware parsing entry points: `parse_file`
//! (single [`SourceFile`] in / [`ParsedFile`] out) and `parse_program`
//! (vec in / [`ParsedProgram`] out).
//!
//! Pins:
//! - `ast.path` and `ast.package` thread through from the input
//!   `SourceFile` to the resulting AST
//! - `parse_program` preserves input order via `ParsedProgram::order`
//! - `ParsedFile::has_errors` discriminates errors from warnings
//! - `ParsedProgram::has_errors` rolls up per-file errors

use std::path::PathBuf;

use expo_parser::{ParseMode, SourceFile, parse_file, parse_program};

#[test]
fn parse_file_threads_path_and_package_into_ast() {
    let src = SourceFile {
        package: "myapp".to_string(),
        path: PathBuf::from("src/main.expo"),
        source: "fn main\n  1\nend\n".to_string(),
    };
    let parsed = parse_file(src, ParseMode::File);
    assert_eq!(parsed.ast.path, Some(PathBuf::from("src/main.expo")));
    assert_eq!(parsed.ast.package, "myapp");
    assert_eq!(parsed.package, "myapp");
    assert_eq!(parsed.path, PathBuf::from("src/main.expo"));
}

#[test]
fn parse_file_has_errors_for_invalid_source() {
    let src = SourceFile {
        package: "broken".to_string(),
        path: PathBuf::from("a.expo"),
        source: "fn foo\n  (1, 2)\nend\n".to_string(),
    };
    let parsed = parse_file(src, ParseMode::File);
    assert!(parsed.has_errors());
}

#[test]
fn parse_file_clean_source_has_no_errors() {
    let src = SourceFile {
        package: "ok".to_string(),
        path: PathBuf::from("a.expo"),
        source: "fn foo\n  1\nend\n".to_string(),
    };
    let parsed = parse_file(src, ParseMode::File);
    assert!(!parsed.has_errors());
}

#[test]
fn parse_program_preserves_input_order() {
    let sources = vec![
        SourceFile {
            package: "p".to_string(),
            path: PathBuf::from("zeta.expo"),
            source: "fn z\n  1\nend\n".to_string(),
        },
        SourceFile {
            package: "p".to_string(),
            path: PathBuf::from("alpha.expo"),
            source: "fn a\n  1\nend\n".to_string(),
        },
        SourceFile {
            package: "p".to_string(),
            path: PathBuf::from("mu.expo"),
            source: "fn m\n  1\nend\n".to_string(),
        },
    ];
    let program = parse_program(sources, ParseMode::File);
    assert_eq!(program.len(), 3);
    assert_eq!(
        program.order,
        vec![
            PathBuf::from("zeta.expo"),
            PathBuf::from("alpha.expo"),
            PathBuf::from("mu.expo"),
        ]
    );
}

#[test]
fn parse_program_iter_walks_in_order() {
    let sources = vec![
        SourceFile {
            package: "p".to_string(),
            path: PathBuf::from("b.expo"),
            source: "fn b\n  1\nend\n".to_string(),
        },
        SourceFile {
            package: "p".to_string(),
            path: PathBuf::from("a.expo"),
            source: "fn a\n  1\nend\n".to_string(),
        },
    ];
    let program = parse_program(sources, ParseMode::File);
    let names: Vec<_> = program.iter().map(|f| f.path.clone()).collect();
    assert_eq!(
        names,
        vec![PathBuf::from("b.expo"), PathBuf::from("a.expo")]
    );
}

#[test]
fn parse_program_has_errors_when_any_file_fails() {
    let sources = vec![
        SourceFile {
            package: "p".to_string(),
            path: PathBuf::from("ok.expo"),
            source: "fn ok\n  1\nend\n".to_string(),
        },
        SourceFile {
            package: "p".to_string(),
            path: PathBuf::from("bad.expo"),
            source: "fn broken\n  (1, 2)\nend\n".to_string(),
        },
    ];
    let program = parse_program(sources, ParseMode::File);
    assert!(program.has_errors());
}

#[test]
fn parse_program_has_no_errors_when_all_clean() {
    let sources = vec![
        SourceFile {
            package: "p".to_string(),
            path: PathBuf::from("a.expo"),
            source: "fn a\n  1\nend\n".to_string(),
        },
        SourceFile {
            package: "p".to_string(),
            path: PathBuf::from("b.expo"),
            source: "fn b\n  2\nend\n".to_string(),
        },
    ];
    let program = parse_program(sources, ParseMode::File);
    assert!(!program.has_errors());
}

#[test]
fn parse_program_get_finds_files_by_path() {
    let sources = vec![SourceFile {
        package: "p".to_string(),
        path: PathBuf::from("solo.expo"),
        source: "fn solo\n  1\nend\n".to_string(),
    }];
    let program = parse_program(sources, ParseMode::File);
    assert!(program.get(&PathBuf::from("solo.expo")).is_some());
    assert!(program.get(&PathBuf::from("missing.expo")).is_none());
}

#[test]
fn parse_program_empty_input() {
    let program = parse_program(vec![], ParseMode::File);
    assert!(program.is_empty());
    assert_eq!(program.len(), 0);
    assert!(!program.has_errors());
}
