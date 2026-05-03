//! Shared helpers for the `expo-ir-eval` integration tests.
//!
//! Every per-language-section test file (`control_flow.rs`, `enums.rs`,
//! `functions.rs`, `literals.rs`, `operators.rs`, `structs.rs`)
//! declares `mod common;` and pulls in [`eval_entry`] / [`dedent`] from
//! here. The harness deliberately avoids `expo-driver`'s project /
//! stdlib resolution so the tests stay fast and self-contained.

use std::sync::Arc;

use expo_ir::Backend;
use expo_ir_eval::{Interp, Value};

/// Parse `source` into an AST file, asserting no parser errors.
pub fn parse_file(source: &str) -> expo_ast::ast::File {
    let parsed = expo_parser::parse(source);
    assert!(
        parsed.errors.is_empty(),
        "parser errors: {:?}",
        parsed.errors
    );
    parsed.ast
}

/// Strip leading newline + uniform indent from a raw multiline source
/// literal. Mirrors the helper in `expo-typecheck`, `expo-fmt`, and
/// `expo-driver` -- lets test sources be written as naturally
/// indented blocks:
///
/// ```ignore
/// eval_entry(&dedent("
///     fn run -> Int
///       42
///     end
/// "), "run");
/// ```
pub fn dedent(s: &str) -> String {
    let s = s.strip_prefix('\n').unwrap_or(s);
    let min_indent = s
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    s.lines()
        .map(|l| {
            if l.len() >= min_indent {
                &l[min_indent..]
            } else {
                l.trim()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Lower a self-contained file (no stdlib auto-import) and
/// interpret `entry`. Use for tests that only touch primitives.
pub fn eval_entry(source: &str, entry: &str) -> Value {
    let mut file = parse_file(source);
    let type_ctx = expo_typecheck::check(&mut file);
    let files = vec![&file];
    let packages = vec!["__test__"];
    let program = expo_codegen::lower_files(&files, &packages, &type_ctx, "__test__", None)
        .unwrap_or_else(|diags| panic!("lower_files failed: {diags:?}"));
    let mut interp = Interp::new(Arc::new(program), Arc::new(type_ctx)).expect("interp init");
    interp
        .call(&expo_ir::FunctionIdentifier::new(entry), vec![])
        .unwrap_or_else(|e| panic!("interp call failed: {e}"))
}
