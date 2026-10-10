//! Entry-process `receive` coverage: the `after`-timeout path and OS
//! signal -> `Lifecycle` arm delivery. Cross-process business delivery
//! (cast/call) lives in `process_intrinsics.rs`.
//!
//! Signal flags are process-global, so the lifecycle test installs
//! handlers by running a trivial entry first, latches SIGTERM with
//! `raise`, then runs the receive fixture, fully deterministic with no
//! threads. Every eval run drains the signal queue, so a run on a
//! parallel test thread would steal the latched SIGTERM. `SIGNALS`
//! serializes the tests in this binary.

mod common;

use std::sync::{Mutex, MutexGuard};

use common::{PACKAGE, typecheck};
use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_ir::lower_program;
use koja_ir_eval::{Interpreter, RuntimeError, Value};
use koja_parser::ParseMode;

unsafe extern "C" {
    fn raise(signal: i32) -> i32;
}

const SIGTERM: i32 = 15;

/// Held for the whole of each test so no two eval runs in this binary
/// drain the process-global signal flags at the same time.
static SIGNALS: Mutex<()> = Mutex::new(());

/// Take `SIGNALS`, recovering the lock if a failed test poisoned it.
fn serial() -> MutexGuard<'static, ()> {
    SIGNALS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Lower `source` (an entry named `App`) and run it with no args.
fn run_entry(source: &str) -> Result<Value, RuntimeError> {
    let checked = typecheck(&dedent(source), ParseMode::File);
    let entry = Identifier::single(PACKAGE, "App");
    let program = lower_program(&checked, &entry).expect("lowering should succeed");
    Interpreter::run_program(&program, &[])
}

/// Process scaffolding wrapped around a `fn run` body.
fn entry_with_run(run_body: &str) -> String {
    format!(
        r#"
        alias Process.Lifecycle
        alias Process.Step
        alias Process.StopReason

        struct App
        end

        impl Process<(), String, ()> for App
          fn start(config: ()) -> Result<Self, StopReason>
            Result.Ok(App{{}})
          end

          fn handle(self, msg: String, from: Option<ReplyTo<()>>) -> Step<Self>
            Step.Continue(self)
          end

          fn run(self) -> StopReason
            {run_body}
          end
        end
        "#
    )
}

#[test]
fn after_timeout_runs_the_after_body() {
    let _serial = serial();
    let source = entry_with_run(
        "
        receive
          envelope: (String, Option<ReplyTo<()>>) -> StopReason.Shutdown
        after 5
          StopReason.Normal
        end
        ",
    );
    let exit_code = run_entry(&source).expect("entry should run to completion");
    assert_eq!(exit_code, Value::Int(0), "after body should select Normal");
}

#[test]
fn sigterm_delivers_lifecycle_shutdown() {
    let _serial = serial();
    // First run installs the latching signal handlers (run_program
    // installs them before user code), so the raise below latches
    // a flag instead of killing the test process.
    let install_only = entry_with_run("StopReason.Normal");
    run_entry(&install_only).expect("trivial entry should run");

    unsafe { raise(SIGTERM) };

    let source = entry_with_run(
        "
        receive
          event: Lifecycle ->
            match event
              Lifecycle.Shutdown -> StopReason.Shutdown
              _ -> StopReason.Normal
            end
        after 5000
          StopReason.Normal
        end
        ",
    );
    let exit_code = run_entry(&source).expect("entry should run to completion");
    assert_eq!(
        exit_code,
        Value::Int(1),
        "pending SIGTERM should dispatch the Lifecycle arm as Shutdown",
    );
}
