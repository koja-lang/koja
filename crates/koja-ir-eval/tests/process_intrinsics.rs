//! `Ref` / `ReplyTo` intrinsic coverage under the cooperative scheduler:
//! cast + call round-trips through a spawned process, the reply-token
//! await, the `Process.CallError.Timeout` / `Process.CallError.ProcessDown` mappings, and
//! `send_after` timer delivery. Each fixture boots a real `Process` entry
//! (`App`) as PID 1 via `Interpreter::run_program` and encodes its
//! assertion in the exit code (`Process.StopReason.Normal` → 0,
//! `Process.StopReason.Shutdown` → 1).

mod common;

use common::{PACKAGE, typecheck};
use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_ir::lower_program;
use koja_ir_eval::{Interpreter, Value};
use koja_parser::ParseMode;

/// Lower `source` (an entry named `App`) and run it with no args.
fn run_app(source: &str) -> Value {
    let checked = typecheck(&dedent(source), ParseMode::File);
    let entry = Identifier::new(PACKAGE, vec!["App".to_string()]);
    let program = lower_program(&checked, &entry).expect("lowering should succeed");
    Interpreter::run_program(&program, &[]).expect("entry should run to completion")
}

/// A stateful worker: each delivered message adds to a running balance,
/// and (for a `call`) replies with the new total. `cast` delivers with
/// `from = None`, `call` with `from = Some`.
const BANK: &str = "
    struct Bank
      balance: Int
    end

    impl Process<Int, Int, Int> for Bank
      fn start(config: Int) -> Result<Bank, Process.StopReason>
        Result.Ok(Bank{balance: config})
      end

      fn handle(self, msg: Int, from: Option<ReplyTo<Int>>) -> Process.Step<Bank>
        total = self.balance + msg
        match from
          Option.Some(r) -> r.send(total)
          Option.None -> ()
        end
        Process.Step.Continue(Bank{balance: total})
      end
    end
    ";

/// A worker that never replies — used to drive the call timeout path.
const MUTE: &str = "
    struct Mute
    end

    impl Process<Int, Int, Int> for Mute
      fn start(config: Int) -> Result<Mute, Process.StopReason>
        Result.Ok(Mute{})
      end

      fn handle(self, msg: Int, from: Option<ReplyTo<Int>>) -> Process.Step<Mute>
        Process.Step.Continue(self)
      end
    end
    ";

#[test]
fn cast_then_call_round_trips_through_spawned_process() {
    let source = format!(
        "{BANK}

        struct App
        end

        impl Process<(), (), ()> for App
          fn start(config: ()) -> Result<App, Process.StopReason>
            Result.Ok(App{{}})
          end

          fn handle(self, msg: (), from: Option<ReplyTo<()>>) -> Process.Step<App>
            Process.Step.Continue(self)
          end

          fn run(self) -> Process.StopReason
            bank = spawn Bank.start(0)
            bank.cast(10)
            bank.cast(5)
            result = bank.call(100, 1000)
            match result
              Result.Ok(total) ->
                if total == 115
                  Process.StopReason.Normal
                else
                  Process.StopReason.Shutdown
                end
              Result.Err(_) -> Process.StopReason.Shutdown
            end
          end
        end
        "
    );
    assert_eq!(
        run_app(&source),
        Value::Int(0),
        "two casts (10, 5) then a call(100) should reply with the running total 115",
    );
}

#[test]
fn call_to_silent_process_times_out() {
    let source = format!(
        "{MUTE}

        struct App
        end

        impl Process<(), (), ()> for App
          fn start(config: ()) -> Result<App, Process.StopReason>
            Result.Ok(App{{}})
          end

          fn handle(self, msg: (), from: Option<ReplyTo<()>>) -> Process.Step<App>
            Process.Step.Continue(self)
          end

          fn run(self) -> Process.StopReason
            worker = spawn Mute.start(0)
            result = worker.call(1, 25)
            match result
              Result.Ok(_) -> Process.StopReason.Normal
              Result.Err(error) ->
                match error
                  Process.CallError.Timeout -> Process.StopReason.Shutdown
                  Process.CallError.ProcessDown -> Process.StopReason.Normal
                end
            end
          end
        end
        "
    );
    assert_eq!(
        run_app(&source),
        Value::Int(1),
        "a call to a process that never replies should surface Process.CallError.Timeout",
    );
}

#[test]
fn call_to_dead_process_reports_process_down() {
    let source = format!(
        "{MUTE}

        struct App
        end

        impl Process<(), (), ()> for App
          fn start(config: ()) -> Result<App, Process.StopReason>
            Result.Ok(App{{}})
          end

          fn handle(self, msg: (), from: Option<ReplyTo<()>>) -> Process.Step<App>
            Process.Step.Continue(self)
          end

          fn run(self) -> Process.StopReason
            worker = spawn Mute.start(0)
            worker.kill()
            result = worker.call(1, 25)
            match result
              Result.Ok(_) -> Process.StopReason.Normal
              Result.Err(error) ->
                match error
                  Process.CallError.Timeout -> Process.StopReason.Normal
                  Process.CallError.ProcessDown -> Process.StopReason.Shutdown
                end
            end
          end
        end
        "
    );
    assert_eq!(
        run_app(&source),
        Value::Int(1),
        "a call to a killed process should surface Process.CallError.ProcessDown",
    );
}

#[test]
fn send_after_delivers_a_delayed_business_message() {
    let source = "
        struct App
        end

        impl Process<(), Int, ()> for App
          fn start(config: ()) -> Result<App, Process.StopReason>
            Result.Ok(App{})
          end

          fn handle(self, msg: Int, from: Option<ReplyTo<()>>) -> Process.Step<App>
            Process.Step.Continue(self)
          end

          fn run(self) -> Process.StopReason
            me: Ref<Int, ()> = Ref.self_ref()
            me.send_after(42, 1)
            receive
              pair: Pair<Int, Option<ReplyTo<()>>> ->
                if pair.first == 42
                  Process.StopReason.Normal
                else
                  Process.StopReason.Shutdown
                end
              event: Process.Lifecycle -> Process.StopReason.Shutdown
            after 1000
              Process.StopReason.Shutdown
            end
          end
        end
        ";
    assert_eq!(
        run_app(source),
        Value::Int(0),
        "send_after should deliver the delayed business message to the receive loop",
    );
}
