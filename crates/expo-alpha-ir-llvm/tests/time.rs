//! IR-text snapshot tests for the auto-imported `Global.time`
//! stdlib file. Pins the two halves of the slice's contract:
//!
//! - The `@extern "C" priv fn expo_time_now_millis -> Int64` inside
//!   `DateTime` lands as a bare `declare i64 @expo_time_now_millis()`
//!   so the linker resolves against `expo-runtime`'s exported C
//!   symbol (`expo/crates/expo-runtime/src/system.rs`).
//! - `DateTime.now()` calls into the extern from a non-extern body,
//!   so the user-facing call site for `DateTime.now()` routes
//!   through the alpha-mangled `Global.DateTime.now` symbol that in
//!   turn invokes the C-named extern.
//! - The pure-Expo getters (`Duration.from_millis(.)`,
//!   `Duration.millis(self)`, `DateTime.timestamp_millis(self)`)
//!   lower as ordinary functions; their bodies use `i64` everywhere
//!   because alpha treats `Int` and `Int64` interchangeably.

use expo_alpha_ir_llvm::emit_script_llvm_ir;
use expo_ast::util::dedent;

mod common;

use common::{APP_NAME, assert_contains, lower_script_source as lower_as_script};

fn emit(source: &str) -> String {
    let script = lower_as_script(&dedent(source));
    emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed")
}

#[test]
fn datetime_now_call_emits_extern_declare_for_runtime_symbol() {
    // Triggering `DateTime.now()` (transitively, via
    // `.timestamp_millis()` so the script trailing value is a
    // primitive the auto-print scaffolding accepts) forces the
    // emitter to declare `expo_time_now_millis` — the C-named
    // extern backing the call — so it's resolvable at link time
    // against `expo-runtime`.
    let ir_text = emit("DateTime.now().timestamp_millis()");

    assert_contains(&ir_text, "declare i64 @expo_time_now_millis()");
}

#[test]
fn datetime_now_does_not_re_emit_runtime_symbol_under_alpha_mangling() {
    // The extern's link name is the function's bare last-segment
    // (`expo_time_now_millis`), not the alpha-mangled
    // `Global.DateTime.expo_time_now_millis`. Mirror the assertion
    // shape from `extern.rs`: confirm there's no alpha-mangled
    // declare leaking in alongside.
    let ir_text = emit("DateTime.now().timestamp_millis()");

    assert!(
        !ir_text.contains("@Global.DateTime.expo_time_now_millis"),
        "extern declaration must use the bare C name, not the alpha mangling; got:\n{ir_text}",
    );
}

#[test]
fn duration_from_millis_pure_expo_body_lowers_with_i64() {
    // `Duration.from_millis(ms)` is pure-Expo — body just builds a
    // `Duration` struct from the param. Pin the function shape so
    // any drift in struct lowering or param threading shows up.
    // Project to `.millis()` so the script trailing is a primitive.
    let ir_text = emit("Duration.from_millis(1500).millis()");

    assert_contains(&ir_text, "define ");
    assert_contains(&ir_text, "@Global.Duration.from_millis");
    assert!(
        !ir_text.contains("declare i64 @Global.Duration.from_millis"),
        "pure-Expo function must emit a body, not just a declare; got:\n{ir_text}",
    );
}

#[test]
fn duration_millis_getter_lowers_to_field_load() {
    // `Duration.millis(self)` is a single field read. Verify the
    // function exists and returns `i64` (Expo `Int = i64`).
    let ir_text = emit("Duration.from_millis(42).millis()");

    assert_contains(&ir_text, "define i64 @Global.Duration.millis");
}

#[test]
fn datetime_timestamp_millis_lowers_to_field_load() {
    let ir_text = emit("DateTime.now().timestamp_millis()");

    assert_contains(&ir_text, "define i64 @Global.DateTime.timestamp_millis");
}
