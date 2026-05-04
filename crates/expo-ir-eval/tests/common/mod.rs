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
use expo_parser::ParseMode;

pub use expo_ast::util::dedent;

/// Parse `source` into an AST file, asserting no parser errors.
pub fn parse_file(source: &str) -> expo_ast::ast::File {
    let parsed = expo_parser::parse(source, ParseMode::File);
    assert!(
        parsed.errors.is_empty(),
        "parser errors: {:?}",
        parsed.errors
    );
    parsed.ast
}

/// Lower a self-contained file (no stdlib auto-import) and
/// interpret `entry`. Use for tests that only touch primitives.
pub fn eval_entry(source: &str, entry: &str) -> Value {
    let mut file = parse_file(source);
    file.package = "__test__".to_string();
    let type_ctx = expo_typecheck::check(&mut file);
    let files = vec![&file];
    let program = expo_codegen::lower_files(&files, &type_ctx, "__test__", None)
        .unwrap_or_else(|diags| panic!("lower_files failed: {diags:?}"));
    let mut interp = Interp::new(Arc::new(program), Arc::new(type_ctx)).expect("interp init");
    interp
        .call(&expo_ir::FunctionIdentifier::new(entry), vec![])
        .unwrap_or_else(|e| panic!("interp call failed: {e}"))
}
