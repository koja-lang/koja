//! Runtime coverage for monomorphized generic decls.
//!
//! Pins the contract that a `Pair<Int, String>` construction reaches
//! the interpreter as a [`Value::Struct`] tagged with the mangled
//! monomorphized [`IRSymbol`] (`TestApp.Pair_$Int64.String$`), and a
//! generic-enum construction (`Box.Of(42)`) reaches it as a
//! [`Value::Enum`] tagged with `TestApp.Box_$Int64$`. The
//! interpreter has no generics-aware code path: the symbol is
//! whatever the IR `EnumConstruct` / `StructInit` carries, so a
//! green test here also pins that monomorphization is fully
//! resolved by the time the IR reaches eval.
//!
//! Distinct args round-tripping through eval with distinct symbols
//! makes the dedup-by-instantiation-set contract observable — every
//! value reaching the interpreter carries the mangled name, so the
//! test fixture observes the closure-pass result end-to-end without
//! reaching back into [`expo_alpha_ir`] internals.
//!
//! Field-access *through* a generic value (`Pair{...}.a`) isn't
//! exercised here: typecheck doesn't yet substitute `TypeParam`
//! into a `FieldAccess`'s result resolution, so those programs hit
//! a seal violation upstream of IR. They land with a future
//! typecheck slice; the IR contract is concretely pinned by the
//! construction-shaped tests in this file.

use expo_alpha_ir_eval::{EnumPayload, Value};
use expo_ast::util::dedent;

mod common;

fn evaluate_script(source: &str) -> Value {
    common::evaluate_script(source).expect("interpreter should not error on this fixture")
}

// ---------------------------------------------------------------------------
// Generic structs
// ---------------------------------------------------------------------------

#[test]
fn generic_struct_construction_yields_value_struct_with_mangled_symbol() {
    let source = "
        struct Pair<T, U>
          a: T
          b: U
        end

        Pair{a: 1, b: \"x\"}
        ";

    let value = evaluate_script(&dedent(source));
    let Value::Struct { symbol, fields } = value else {
        panic!("expected Value::Struct, got {value:?}");
    };
    assert_eq!(symbol.mangled(), "TestApp.Pair_$Int64.String$");
    assert_eq!(fields, vec![Value::Int(1), Value::String("x".to_string())],);
}

#[test]
fn generic_struct_distinct_args_round_trip_with_distinct_symbols() {
    let int_string = evaluate_script(&dedent(
        "
        struct Pair<T, U>
          a: T
          b: U
        end

        Pair{a: 1, b: \"x\"}
        ",
    ));
    let string_int = evaluate_script(&dedent(
        "
        struct Pair<T, U>
          a: T
          b: U
        end

        Pair{a: \"y\", b: 2}
        ",
    ));

    let Value::Struct { symbol: a, .. } = int_string else {
        panic!("expected Value::Struct");
    };
    let Value::Struct { symbol: b, .. } = string_int else {
        panic!("expected Value::Struct");
    };
    assert_eq!(a.mangled(), "TestApp.Pair_$Int64.String$");
    assert_eq!(b.mangled(), "TestApp.Pair_$String.Int64$");
}

// ---------------------------------------------------------------------------
// Generic enums
// ---------------------------------------------------------------------------

#[test]
fn generic_enum_tuple_variant_construction_yields_value_enum_with_mangled_symbol() {
    let source = "
        enum Box<T>
          Of(T)
        end

        Box.Of(42)
        ";

    let value = evaluate_script(&dedent(source));
    let Value::Enum {
        symbol,
        name,
        tag,
        payload,
    } = value
    else {
        panic!("expected Value::Enum, got {value:?}");
    };
    assert_eq!(symbol.mangled(), "TestApp.Box_$Int64$");
    assert_eq!(name, "Of");
    assert_eq!(tag.0, 0);
    assert_eq!(payload, EnumPayload::Tuple(vec![Value::Int(42)]));
}

#[test]
fn generic_enum_struct_variant_carries_named_payload_fields() {
    let source = "
        enum Pair<T, U>
          Of { a: T, b: U }
        end

        Pair.Of{a: 1, b: \"x\"}
        ";

    let value = evaluate_script(&dedent(source));
    let Value::Enum {
        symbol, payload, ..
    } = value
    else {
        panic!("expected Value::Enum, got {value:?}");
    };
    assert_eq!(symbol.mangled(), "TestApp.Pair_$Int64.String$");
    assert_eq!(
        payload,
        EnumPayload::Struct(vec![
            ("a".to_string(), Value::Int(1)),
            ("b".to_string(), Value::String("x".to_string())),
        ]),
    );
}

#[test]
fn generic_enum_distinct_args_round_trip_with_distinct_symbols() {
    let with_int = evaluate_script(&dedent(
        "
        enum Box<T>
          Of(T)
        end

        Box.Of(42)
        ",
    ));
    let with_string = evaluate_script(&dedent(
        "
        enum Box<T>
          Of(T)
        end

        Box.Of(\"x\")
        ",
    ));

    let Value::Enum { symbol: a, .. } = with_int else {
        panic!("expected Value::Enum");
    };
    let Value::Enum { symbol: b, .. } = with_string else {
        panic!("expected Value::Enum");
    };
    assert_eq!(a.mangled(), "TestApp.Box_$Int64$");
    assert_eq!(b.mangled(), "TestApp.Box_$String$");
}
