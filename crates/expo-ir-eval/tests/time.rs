//! Eval coverage for the auto-imported `Global.time` stdlib file.
//! The pure-Expo bodies (`Duration.from_secs` / `from_millis` /
//! `millis`, `DateTime.timestamp_millis`) evaluate end-to-end on
//! the interpreter; the `@extern "C" priv fn expo_time_now_millis`
//! routes through `expo-ir-eval`'s curated extern dispatch
//! table, which calls into `expo-runtime`'s `expo_time_now_millis`
//! over the C ABI — the same symbol the LLVM backend would link
//! against, so the two backends observe identical wall-clock
//! values.

use expo_ast::util::dedent;
use expo_ir_eval::{RuntimeError, Value};

mod common;

use common::evaluate_script;

fn run_int(source: &str) -> i64 {
    match evaluate_script(&dedent(source)).unwrap() {
        Value::Int(v) => v,
        other => panic!("expected Value::Int, got {other:?}"),
    }
}

#[test]
fn duration_from_secs_multiplies_by_thousand() {
    // `Duration.from_secs(3)` should construct a `Duration` whose
    // `millis = 3000`; project to a primitive via `.millis()` so
    // the script trailing is an `Int` the harness can read.
    let v = run_int("Duration.from_secs(3).millis()");
    assert_eq!(v, 3_000);
}

#[test]
fn duration_from_millis_passes_through() {
    let v = run_int("Duration.from_millis(1500).millis()");
    assert_eq!(v, 1500);
}

#[test]
fn datetime_timestamp_millis_returns_underlying_field() {
    // Build a `DateTime` directly so we pin the pure-Expo getter
    // independent of the wall clock.
    let v = run_int("DateTime{millis: 42}.timestamp_millis()");
    assert_eq!(v, 42);
}

#[test]
fn datetime_now_calls_runtime_extern_for_wall_clock() {
    // `DateTime.now()` lowers to a call into `priv @extern "C" fn
    // expo_time_now_millis`. The eval extern table routes the C
    // symbol straight into `expo-runtime`, so the result is a
    // positive `Int` reflecting the live wall clock.
    let v = run_int("DateTime.now().timestamp_millis()");
    assert!(
        v > 0,
        "expected positive epoch-millis from runtime extern; got {v}",
    );
}

#[test]
fn unknown_extern_surfaces_as_extern_not_supported() {
    // Sanity-pin the negative path: an `@extern "C"` whose C
    // symbol isn't registered in the eval dispatch table still
    // surfaces an explicit error rather than silently returning
    // `Unit` or panicking.
    let source = dedent(
        r#"
        @extern "C"
        fn unregistered_runtime_symbol -> Int64

        unregistered_runtime_symbol()
        "#,
    );
    let err = evaluate_script(&source)
        .expect_err("calling an unregistered extern from eval should fail at runtime");
    match err {
        RuntimeError::ExternNotSupported { symbol } => {
            assert!(
                symbol.contains("unregistered_runtime_symbol"),
                "expected ExternNotSupported to mention the symbol; got `{symbol}`",
            );
        }
        other => panic!("expected ExternNotSupported, got {other:?}"),
    }
}
