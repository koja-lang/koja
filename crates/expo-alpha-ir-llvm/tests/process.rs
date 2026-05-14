//! Coverage for the `IRInstruction::Spawn` / `IRInstruction::Receive`
//! emit paths in [`expo_alpha_ir_llvm::emit::process`] and the
//! `Ref<M, R>` / `ReplyTo<R>` intrinsic emitters in
//! [`expo_alpha_ir_llvm::intrinsics::process`]. Pins:
//!
//! - **Spawn**: serializes config to a stack alloca, calls
//!   `expo_rt_spawn(wrapper, blob, size)`, wraps the returned pid
//!   in the `Ref<M, R>` struct.
//! - **SpawnWrapper body**: declared as `void(i8*)`; loads the
//!   typed config, calls `<state>.start`, branches on the
//!   `Result.tag`, chains into `<state>.run` on Ok.
//! - **Receive**: calls `expo_rt_receive` (or
//!   `expo_rt_receive_timeout` with the `after` clause), reads the
//!   envelope tag, dispatches to per-arm "deserialize then branch"
//!   blocks ending in `unreachable` for unmatched tags.
//! - **Ref intrinsics**: `self_ref`, `cast`, `signal`, `kill`,
//!   `alive?`, `send_after`, plus `ReplyTo.send` — each routes
//!   through the matching `expo_rt_*` extern declared in
//!   [`expo_alpha_ir_llvm::runtime`].
//!
//! Tests inline minimal `Lifecycle` / `StopReason` / `Step` /
//! `ReplyTo` / `Ref` / `Process` definitions so the suite doesn't
//! depend on `Global.process` being autoimported (that step lands
//! later in the alpha-concurrency-process plan).

use std::path::PathBuf;

use expo_alpha_ir::{IRProgram, lower_program};
use expo_alpha_ir_llvm::emit_llvm_ir;
use expo_alpha_typecheck::check_program;
use expo_ast::identifier::Identifier;
use expo_ast::util::dedent;
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";
const APP_NAME: &str = "alpha_process_test";

/// Minimal alpha-friendly stub of `process.expo`. Mirrors the
/// stubs used by `expo-alpha-ir/tests/lower_process.rs` and
/// `expo-alpha-typecheck/tests/process.rs`.
const PROCESS_STUB: &str = "
    enum Lifecycle
      Shutdown
      Interrupt
      Reload
    end

    enum StopReason
      Normal
      Shutdown
    end

    enum CallError
      Timeout
      ProcessDown
    end

    enum Step<S>
      Continue(S)
      Done(StopReason)
    end

    struct ReplyTo<R>
      id: Int
    end

    impl ReplyTo<R>
      @intrinsic
      fn send(self, reply: R)
    end

    struct Ref<M, R>
      id: Int
    end

    impl Ref<M, R>
      @intrinsic
      fn self_ref -> Ref<M, R>

      @intrinsic
      fn cast(self, msg: M)

      @intrinsic
      fn signal(self, event: Lifecycle)

      @intrinsic
      fn kill(self)

      @intrinsic
      fn alive?(self) -> Bool

      @intrinsic
      fn send_after(self, msg: M, delay_ms: Int)

      @intrinsic
      fn call(self, msg: M, timeout_ms: Int) -> Result<R, CallError>
    end

    protocol Process<C, M, R>
      fn start(move config: C) -> Result<Self, StopReason>
      fn handle(move self, msg: M, from: Option<ReplyTo<R>>) -> Step<Self>
      fn run(move self) -> StopReason
    end
    ";

fn lower(source: &str) -> IRProgram {
    let mut sources = expo_stdlib::alpha_autoimport_sources();
    sources.push(SourceFile {
        package: "Global".to_string(),
        path: PathBuf::from("<Global.process>"),
        source: dedent(PROCESS_STUB),
    });
    sources.push(SourceFile {
        package: PACKAGE.to_string(),
        path: PathBuf::from("test.expo"),
        source: dedent(source),
    });
    let parsed = parse_program(sources, ParseMode::File);
    let checked = check_program(parsed).unwrap_or_else(|failure| {
        panic!(
            "alpha typecheck failed: {} diagnostic(s):\n{}",
            failure.diagnostics.len(),
            failure
                .diagnostics
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        )
    });
    let entry = Identifier::new(PACKAGE, vec!["main".to_string()]);
    lower_program(&checked, entry).expect("lowering should succeed")
}

fn emit(source: &str) -> String {
    let program = lower(source);
    emit_llvm_ir(&program, APP_NAME).expect("LLVM emit should succeed")
}

fn assert_contains(ir_text: &str, needle: &str) {
    assert!(
        ir_text.contains(needle),
        "expected `{needle}` in:\n{ir_text}",
    );
}

const COUNTER_PROCESS: &str = "
    struct Counter
      count: Int
    end

    impl Process<Int, Int, Int> for Counter
      fn start(move config: Int) -> Result<Counter, StopReason>
        Result.Ok(Counter{count: config})
      end

      fn handle(move self, msg: Int, from: Option<ReplyTo<Int>>) -> Step<Counter>
        Step.Done(StopReason.Normal)
      end

      fn run(move self) -> StopReason
        receive
          event: Lifecycle ->
            StopReason.Shutdown
        end
      end
    end
    ";

#[test]
fn spawn_calls_expo_rt_spawn_with_wrapper_and_serialized_config() {
    let mut source = String::from(COUNTER_PROCESS);
    source.push_str(
        "
        fn main
          handle = spawn Counter.start(0)
        end
        ",
    );
    let ir_text = emit(&source);

    assert_contains(&ir_text, "declare i64 @expo_rt_spawn(ptr, ptr, i64)");
    // Unquoted symbol form (no `_$..$` suffix means inkwell skips
    // the quoting). Asserts the wrapper symbol is the one fed to
    // `expo_rt_spawn`, the config blob is freshly allocated on the
    // host stack, and the literal config (`0`) reaches the buffer
    // before the call.
    assert_contains(
        &ir_text,
        "call i64 @expo_rt_spawn(ptr @TestApp.Counter.__spawn_wrapper",
    );
    assert_contains(&ir_text, "alloca i64");
    assert_contains(&ir_text, "store i64 0");
}

#[test]
fn spawn_wrapper_loads_config_calls_start_then_run_on_ok() {
    let mut source = String::from(COUNTER_PROCESS);
    source.push_str(
        "
        fn main
          handle = spawn Counter.start(0)
        end
        ",
    );
    let ir_text = emit(&source);

    assert_contains(&ir_text, "define void @TestApp.Counter.__spawn_wrapper(ptr");
    assert_contains(&ir_text, "loaded_config");
    // start returns Result<Counter, StopReason>; the alpha mangler
    // qualifies StopReason with the package it was lifted from
    // (today: `Global`, since the protocol stub lifts every type
    // declaration into the `Global` package).
    assert_contains(
        &ir_text,
        "call %\"Global.Result_$TestApp.Counter.Global.StopReason$\" @TestApp.Counter.start",
    );
    assert_contains(&ir_text, "is_ok");
    assert_contains(&ir_text, "start_ok:");
    assert_contains(&ir_text, "start_err:");
    assert_contains(&ir_text, "call %Global.StopReason @TestApp.Counter.run");
}

#[test]
fn receive_lifecycle_calls_expo_rt_receive_and_dispatches_on_tag() {
    let source = "
        fn drain -> StopReason
          receive
            event: Lifecycle ->
              StopReason.Shutdown
          end
        end

        fn main
          drain()
        end
        ";
    let ir_text = emit(source);

    assert_contains(&ir_text, "declare ptr @expo_rt_receive()");
    assert_contains(&ir_text, "call ptr @expo_rt_receive()");
    assert_contains(&ir_text, "envelope_tag");
    assert_contains(&ir_text, "is_arm_0");
    // Each arm body block lives in the function's CFG and the
    // dispatch jumps in via the per-arm prelude block. The host
    // block of the receive ends with a conditional branch — its
    // IR-level Unreachable terminator becomes the fallthrough
    // unreachable after the arm tests.
    assert_contains(&ir_text, "lifecycle_payload");
}

#[test]
fn receive_with_after_calls_receive_timeout_and_branches_on_null() {
    let source = "
        fn drain -> StopReason
          receive
            event: Lifecycle ->
              StopReason.Shutdown
          after 100
            StopReason.Normal
          end
        end

        fn main
          drain()
        end
        ";
    let ir_text = emit(source);

    assert_contains(&ir_text, "declare ptr @expo_rt_receive_timeout(i64)");
    assert_contains(&ir_text, "call ptr @expo_rt_receive_timeout(i64 100)");
    assert_contains(&ir_text, "envelope_is_null");
}

#[test]
fn ref_self_ref_emits_expo_rt_self_wrapped_in_ref_struct() {
    // self_ref is only callable from inside a process body; reach
    // it via a helper method on the Counter state so the
    // monomorphized Ref<Int, Int>.self_ref intrinsic gets emitted.
    let mut source = String::from(COUNTER_PROCESS);
    source.push_str(
        "
        impl Counter
          fn whoami -> Ref<Int, Int>
            Ref.self_ref()
          end
        end

        fn main
          handle = spawn Counter.start(0)
        end
        ",
    );
    let ir_text = emit(&source);

    assert_contains(&ir_text, "declare i64 @expo_rt_self()");
    assert_contains(&ir_text, "call i64 @expo_rt_self()");
}

#[test]
fn ref_signal_loads_lifecycle_variant_byte_and_calls_send_lifecycle() {
    let mut source = String::from(COUNTER_PROCESS);
    source.push_str(
        "
        fn main
          handle = spawn Counter.start(0)
          handle.signal(Lifecycle.Shutdown)
        end
        ",
    );
    let ir_text = emit(&source);

    assert_contains(&ir_text, "declare void @expo_rt_send_lifecycle(i64, i64)");
    assert_contains(&ir_text, "call void @expo_rt_send_lifecycle(i64");
    assert_contains(&ir_text, "variant_byte");
}

#[test]
fn ref_cast_emits_pair_envelope_with_none_reply_to_and_calls_expo_rt_send() {
    let mut source = String::from(COUNTER_PROCESS);
    source.push_str(
        "
        fn main
          handle = spawn Counter.start(0)
          handle.cast(42)
        end
        ",
    );
    let ir_text = emit(&source);

    assert_contains(&ir_text, "declare void @expo_rt_send(i64, ptr, i64)");
    assert_contains(&ir_text, "call void @expo_rt_send(i64");
    assert_contains(&ir_text, "cast_envelope");
    assert_contains(&ir_text, "pair_msg");
    assert_contains(&ir_text, "pair_option");
    // The Pair envelope packs `Option::None` as `[i64 1, i64 0]`
    // (tag byte = 1 in little-endian first lane, padding word
    // zero), independent of `R`. Pinning the literal here
    // catches accidental tag-flip regressions.
    assert_contains(&ir_text, "[2 x i64] [i64 1, i64 0]");
}

#[test]
fn ref_kill_calls_expo_rt_kill_with_pid() {
    let mut source = String::from(COUNTER_PROCESS);
    source.push_str(
        "
        fn main
          handle = spawn Counter.start(0)
          handle.kill()
        end
        ",
    );
    let ir_text = emit(&source);

    assert_contains(&ir_text, "declare void @expo_rt_kill(i64)");
    assert_contains(&ir_text, "call void @expo_rt_kill(i64");
}

#[test]
fn ref_alive_compares_expo_rt_is_process_alive_against_zero() {
    let mut source = String::from(COUNTER_PROCESS);
    source.push_str(
        "
        fn main
          handle = spawn Counter.start(0)
          alive = handle.alive?()
        end
        ",
    );
    let ir_text = emit(&source);

    assert_contains(&ir_text, "declare i64 @expo_rt_is_process_alive(i64)");
    assert_contains(&ir_text, "is_alive");
}

#[test]
fn ref_send_after_emits_pair_envelope_and_passes_delay_to_runtime() {
    let mut source = String::from(COUNTER_PROCESS);
    source.push_str(
        "
        fn main
          handle = spawn Counter.start(0)
          handle.send_after(7, 250)
        end
        ",
    );
    let ir_text = emit(&source);

    assert_contains(
        &ir_text,
        "declare void @expo_rt_send_after(i64, ptr, i64, i64)",
    );
    assert_contains(&ir_text, "call void @expo_rt_send_after(i64");
    assert_contains(&ir_text, "i64 250");
    // Same `Pair<M, Option<ReplyTo<R>>>` envelope as `Ref.cast`,
    // with `Option::None` in the reply slot (the runtime delivers
    // the message into the same mailbox the receive arm reads).
    assert_contains(&ir_text, "send_after_envelope");
    assert_contains(&ir_text, "[2 x i64] [i64 1, i64 0]");
}

/// Pins the Unit-as-msg-payload codegen surface used by
/// `Task<R>` (where the public-API `Ref<(), R>` pins `M = Unit`).
/// Unit at the IR layer has no value-level type — the LLVM
/// boundary maps it to an `i8` placeholder in every value
/// position (param, struct field, local) so the Pair envelope
/// still lays out cleanly. Catches regressions in any of:
/// `function::function_signature`, `types::value_basic_type`,
/// or the `Ref.cast` intrinsic's `value_basic_type` lookup.
#[test]
fn ref_cast_with_unit_message_uses_i8_placeholder_in_envelope() {
    let unit_process = "
        struct UnitWorker
          tag: Int
        end

        impl Process<Int, (), Int> for UnitWorker
          fn start(move config: Int) -> Result<UnitWorker, StopReason>
            Result.Ok(UnitWorker{tag: config})
          end

          fn handle(move self, msg: (), from: Option<ReplyTo<Int>>) -> Step<UnitWorker>
            Step.Done(StopReason.Normal)
          end

          fn run(move self) -> StopReason
            receive
              event: Lifecycle ->
                StopReason.Shutdown
            end
          end
        end
        ";
    let mut source = String::from(unit_process);
    source.push_str(
        "
        fn main
          handle = spawn UnitWorker.start(0)
          handle.cast(())
        end
        ",
    );
    let ir_text = emit(&source);

    // Signature carries the i8 placeholder where M = Unit lands;
    // the Pair envelope still packs an Option::None reply slot in
    // the trailing `[2 x i64]` array.
    assert_contains(
        &ir_text,
        "define void @\"Global.Ref_$Unit.Int64$.cast\"(%\"Global.Ref_$Unit.Int64$\" %0, i8 %1)",
    );
    assert_contains(&ir_text, "%cast_envelope = alloca { i8, [2 x i64] }");
    assert_contains(
        &ir_text,
        "%pair_msg = insertvalue { i8, [2 x i64] } undef, i8 %1, 0",
    );
    assert_contains(&ir_text, "[2 x i64] [i64 1, i64 0]");
}

#[test]
fn ref_call_emits_pair_envelope_with_some_reply_to_and_receive_loop() {
    let mut source = String::from(COUNTER_PROCESS);
    source.push_str(
        "
        fn main
          handle = spawn Counter.start(0)
          reply = handle.call(7, 100)
        end
        ",
    );
    let ir_text = emit(&source);

    // Writer side: the call envelope is the same `Pair<M,
    // Option<ReplyTo<R>>>` shape as cast / send_after, but the
    // reply slot is `Option::Some(ReplyTo { id: caller_pid })`
    // — caller pid sourced from `expo_rt_self`, packed as the
    // second word of the option payload. inkwell folds the initial
    // tag insert into the array literal, leaving only the runtime
    // pid insert as a named SSA value.
    assert_contains(&ir_text, "declare i64 @expo_rt_self()");
    assert_contains(&ir_text, "call i64 @expo_rt_self()");
    assert_contains(&ir_text, "call_envelope");
    assert_contains(&ir_text, "[2 x i64] [i64 0, i64 undef]");
    assert_contains(&ir_text, "opt_pid");
    assert_contains(&ir_text, "call void @expo_rt_send(i64");

    // Reader side: paired `expo_rt_receive_timeout` against the
    // caller's own mailbox. Three-way dispatch on the result
    // (timeout / process-down / Ok) feeds a single phi that
    // returns `Result<R, CallError>`.
    assert_contains(&ir_text, "declare ptr @expo_rt_receive_timeout(i64)");
    // The literal `100` is consumed at the `.call` call site;
    // inside the intrinsic body the timeout flows through `%2`
    // (the third parameter).
    assert_contains(&ir_text, "call ptr @expo_rt_receive_timeout(i64 %2)");
    assert_contains(&ir_text, "reply_is_null");
    assert_contains(&ir_text, "call_timeout_check:");
    assert_contains(&ir_text, "call_got_reply:");
    assert_contains(&ir_text, "call_build_timeout:");
    assert_contains(&ir_text, "call_build_down:");
    assert_contains(&ir_text, "call_merge:");
    assert_contains(&ir_text, "target_alive");
    assert_contains(&ir_text, "reply_value");
}
