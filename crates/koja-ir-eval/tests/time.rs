//! Eval coverage for the auto-imported `Global.time` stdlib file.
//! The pure-Koja bodies (`Duration.new` / `to_milliseconds`,
//! `Timestamp.since_epoch`, `Instant.since`) evaluate end-to-end
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
fn duration_in_seconds_converts_to_nanoseconds() {
    let v = run_int("Duration.new(3, Duration.Unit.Seconds).to_nanoseconds()");
    assert_eq!(v, 3_000_000_000);
}

#[test]
fn duration_to_milliseconds_truncates() {
    let v = run_int("Duration.new(1_999_999, Duration.Unit.Nanoseconds).to_milliseconds()");
    assert_eq!(v, 1);
}

#[test]
fn duration_arithmetic_lands_in_the_finer_unit() {
    // `plus` converts both sides to the finer unit, so seconds plus
    // milliseconds is milliseconds. The user-written `equals?` makes
    // the cross-unit `==` hold, which the derived one would not.
    let v = run_int(
        r#"
        sum = Duration.new(2, Duration.Unit.Seconds).plus(Duration.new(500, Duration.Unit.Milliseconds))
        sum == Duration.new(2_500_000, Duration.Unit.Microseconds) ? sum.value : -1
        "#,
    );
    assert_eq!(v, 2_500);
}

#[test]
fn timestamp_since_epoch_returns_underlying_field() {
    // Build a `Timestamp` directly so the getter is pinned
    // independent of the wall clock.
    let v = run_int("Timestamp{microseconds: 42}.since_epoch().to_microseconds()");
    assert_eq!(v, 42);
}

#[test]
fn timestamp_now_calls_runtime_extern_for_wall_clock() {
    // `Timestamp.now()` lowers to a call into `priv @extern "C" fn
    // koja_time_now_microseconds`. The eval extern table routes the
    // C symbol straight into `koja-runtime`, so the result is a
    // positive `Int` reflecting the live wall clock.
    let v = run_int("Timestamp.now().since_epoch().to_microseconds()");
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
        second.since(first).to_nanoseconds()
        "#,
    );
    assert!(v >= 0, "expected a non-negative monotonic delta; got {v}");
}

#[test]
fn instant_since_saturates_at_zero() {
    let v = run_int("Instant{nanoseconds: 5}.since(Instant{nanoseconds: 9}).to_nanoseconds()");
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
