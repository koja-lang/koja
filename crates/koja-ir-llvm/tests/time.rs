//! IR-text snapshot tests for the auto-imported `Global.time`
//! stdlib file. Pins the two halves of the slice's contract:
//!
//! - The two `@extern "C" priv fn` clock reads inside `Timestamp`
//!   and `Instant` land as bare `declare i64 @koja_time_...()` lines
//!   so the linker resolves against `koja-runtime`'s exported C
//!   symbols (`koja/crates/koja-runtime-posix/src/system.rs`).
//! - `Timestamp.now()` and `Instant.now()` call into the externs
//!   from non-extern bodies, so the user-facing call sites route
//!   through the name-mangled `Global.Timestamp.now` and
//!   `Global.Instant.now` symbols that in turn invoke the C-named
//!   externs.
//! - The pure-Koja getters (`Duration.new(., Duration.Unit.Milliseconds)`,
//!   `Duration.to_milliseconds(self)`,
//!   `Timestamp.since_epoch(self)`) lower as ordinary functions.
//!   Their bodies use `i64` everywhere because the pipeline treats
//!   `Int` and `Int64` interchangeably.

use koja_ast::util::dedent;
use koja_ir_llvm::emit_script_llvm_ir;

mod common;

use common::{APP_NAME, assert_contains, lower_script_source as lower_as_script};

fn emit(source: &str) -> String {
    let script = lower_as_script(&dedent(source));
    emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed")
}

#[test]
fn timestamp_now_call_emits_extern_declare_for_runtime_symbol() {
    // Triggering `Timestamp.now()` forces the emitter to declare
    // `koja_time_now_microseconds` (the C-named extern backing the
    // call) so it's resolvable at link time against `koja-runtime`.
    let ir_text = emit("Timestamp.now().since_epoch().to_microseconds()");

    assert_contains(&ir_text, "declare i64 @koja_time_now_microseconds()");
}

#[test]
fn instant_now_call_emits_extern_declare_for_runtime_symbol() {
    let ir_text = emit("Instant.now().elapsed().to_nanoseconds()");

    assert_contains(&ir_text, "declare i64 @koja_time_monotonic_nanoseconds()");
}

#[test]
fn timestamp_now_does_not_re_emit_runtime_symbol_under_name_mangling() {
    // The extern's link name is the function's bare last-segment
    // (`koja_time_now_microseconds`), not the name-mangled
    // `Global.Timestamp.koja_time_now_microseconds`. Mirror the
    // assertion shape from `extern.rs`: confirm there's no
    // name-mangled declare leaking in alongside.
    let ir_text = emit("Timestamp.now().since_epoch().to_microseconds()");

    assert!(
        !ir_text.contains("@Global.Timestamp.koja_time_now_microseconds"),
        "extern declaration must use the bare C name, not the name mangling. Got:\n{ir_text}",
    );
}

#[test]
fn duration_new_pure_koja_body_lowers_with_i64() {
    // `Duration.new(value, unit)` is pure-Koja. The body just builds a
    // `Duration` struct from the two params. Pin the function shape so
    // any drift in struct lowering or param threading shows up.
    // Project to `.to_milliseconds()` so the script trailing is a
    // primitive.
    let ir_text = emit("Duration.new(1500, Duration.Unit.Milliseconds).to_milliseconds()");

    assert_contains(&ir_text, "define ");
    assert_contains(&ir_text, "@\"Global.Duration.new/2\"");
    assert!(
        !ir_text.contains("declare i64 @\"Global.Duration.new/2\""),
        "pure-Koja function must emit a body, not just a declare; got:\n{ir_text}",
    );
}

#[test]
fn duration_to_milliseconds_getter_returns_i64() {
    // `Duration.to_milliseconds(self)` converts the stored `value`
    // from its `unit` through the private `in_unit` helper. Verify the
    // function exists and returns `i64` (Koja `Int = i64`).
    let ir_text = emit("Duration.new(42, Duration.Unit.Milliseconds).to_milliseconds()");

    assert_contains(
        &ir_text,
        "define i64 @\"Global.Duration.to_milliseconds/1\"",
    );
}

#[test]
fn timestamp_since_epoch_wraps_the_field() {
    let ir_text = emit("Timestamp.now().since_epoch().to_microseconds()");

    assert_contains(
        &ir_text,
        "define %Global.Duration @\"Global.Timestamp.since_epoch/1\"",
    );
}
