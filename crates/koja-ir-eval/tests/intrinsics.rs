//! Coverage for the eval-side intrinsic dispatch in
//! `src/intrinsics/`, driven through the full pipeline against the
//! real autoimported stdlib. Byte-for-byte stdout assertions live in
//! the `koja-driver` e2e suite (`lang_suite.rs`), where the whole
//! binary's stdout is captured via `Command::output`.
//!
//! Unregistered intrinsic ids (e.g. `@intrinsic fn missing`) used to
//! surface at runtime as [`RuntimeError::UnknownIntrinsic`]. They
//! now fail at lift time because [`koja_ir::IRIntrinsicId`]'s
//! source-axis mapper returns `None` for paths that aren't part of
//! the registered universe. That contract is exercised by
//! `crates/koja-ir/tests/lower_intrinsics.rs`.

use koja_ast::util::dedent;
use koja_ir_eval::{RuntimeError, Value};

mod common;

const PACKAGE: &str = "Global";

fn evaluate_script(source: &str) -> Result<Value, RuntimeError> {
    common::evaluate_script_in(PACKAGE, source)
}

#[test]
fn string_next_uses_utf8_byte_cursors() {
    let source = "
        match \"éa\".next(0)
          Option.Some((character, cursor)) -> character == \"é\" and cursor == 2
          Option.None -> false
        end
        ";

    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Bool(true),);
}

#[test]
fn string_next_rejects_negative_and_exhausted_cursors() {
    let source = "
        match (\"a\".next(-1), \"a\".next(1))
          (Option.None, Option.None) -> true
          _ -> false
        end
        ";

    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Bool(true),);
}

#[test]
fn map_next_returns_an_entry_and_advanced_cursor() {
    let source = "
        map: Map<String, Int> = [\"a\": 1, \"b\": 2]
        match map.next(0)
          Option.Some(((key, value), cursor)) ->
            map.get(key) == Option.Some(value) and cursor == 1
          Option.None -> false
        end
        ";

    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Bool(true),);
}

#[test]
fn set_next_returns_an_item_and_advanced_cursor() {
    let source = "
        set: Set<String> = [\"a\", \"b\"]
        match set.next(0)
          Option.Some((item, cursor)) -> set.has?(item) and cursor == 1
          Option.None -> false
        end
        ";

    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Bool(true),);
}

#[test]
fn collection_next_rejects_negative_and_exhausted_cursors() {
    let source = "
        map: Map<String, Int> = [\"a\": 1]
        set: Set<String> = [\"a\"]
        match (map.next(-1), map.next(1), set.next(-1), set.next(1))
          (Option.None, Option.None, Option.None, Option.None) -> true
          _ -> false
        end
        ";

    assert_eq!(evaluate_script(&dedent(source)).unwrap(), Value::Bool(true),);
}
