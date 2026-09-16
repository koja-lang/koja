//! Eval coverage for the auto-imported `Global.time` stdlib file.
//! The pure-Koja bodies (`Duration.from_seconds` / `as_milliseconds`,
//! `Timestamp.as_microseconds`, `Instant.since`) evaluate end-to-end
//! on the interpreter. The two `@extern "C"` clock reads route
//! through `koja-ir-eval`'s curated extern dispatch table, which
//! calls into `koja-runtime`'s symbols over the C ABI, the same
//! symbols the LLVM backend links against, so the two backends
//! observe identical clocks.

use koja_ast::util::dedent;
use koja_ir_eval::{RuntimeError, Value};

mod common;

use common::evaluate_script;

fn run_int(source: &str) -> i64 {
    match evaluate_script(&dedent(source)).unwrap() {
        Value::Int(v) => v,
        other => panic!("expected Value::Int, got {other:?}"),
    }
}

#[test]
fn duration_from_seconds_counts_nanoseconds() {
    let v = run_int("Duration.from_seconds(3).as_nanoseconds()");
    assert_eq!(v, 3_000_000_000);
}

#[test]
fn duration_as_milliseconds_truncates() {
    let v = run_int("Duration.from_nanoseconds(1_999_999).as_milliseconds()");
    assert_eq!(v, 1);
}

#[test]
fn timestamp_as_microseconds_returns_underlying_field() {
    // Build a `Timestamp` directly so the getter is pinned
    // independent of the wall clock.
    let v = run_int("Timestamp{microseconds: 42}.as_microseconds()");
    assert_eq!(v, 42);
}

#[test]
fn timestamp_now_calls_runtime_extern_for_wall_clock() {
    // `Timestamp.now()` lowers to a call into `priv @extern "C" fn
    // koja_time_now_microseconds`. The eval extern table routes the
    // C symbol straight into `koja-runtime`, so the result is a
    // positive `Int` reflecting the live wall clock.
    let v = run_int("Timestamp.now().as_microseconds()");
    assert!(
        v > 0,
        "expected positive epoch microseconds from runtime extern; got {v}",
    );
}

#[test]
fn instant_now_calls_runtime_extern_for_monotonic_clock() {
    // Two reads in order never go backwards. The anchor is fixed on
    // the first read in the process, so the value is small and only
    // the difference carries meaning.
    let v = run_int(
        r#"
        first = Instant.now()
        second = Instant.now()
        second.since(first).as_nanoseconds()
        "#,
    );
    assert!(v >= 0, "expected a non-negative monotonic delta; got {v}");
}

#[test]
fn instant_since_saturates_at_zero() {
    let v = run_int("Instant{nanoseconds: 5}.since(Instant{nanoseconds: 9}).as_nanoseconds()");
    assert_eq!(v, 0);
}

#[test]
fn unknown_extern_surfaces_as_extern_unresolved() {
    // Sanity-pin the negative path: an `@extern "C"` whose C
    // symbol is neither shimmed nor findable by the loader still
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
        RuntimeError::ExternUnresolved { symbol, .. } => {
            assert!(
                symbol.contains("unregistered_runtime_symbol"),
                "expected ExternUnresolved to mention the symbol; got `{symbol}`",
            );
        }
        other => panic!("expected ExternUnresolved, got {other:?}"),
    }
}
