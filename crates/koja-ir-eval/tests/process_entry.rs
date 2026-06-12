//! `Interpreter::run_program` process-entry coverage: the
//! argv-shaped `List<String>` config materializes from the caller's
//! `args` (mirroring the LLVM trampoline's `koja_rt_build_argv`),
//! and the entry body's `StopReason` round-trips to the exit code
//! `Value::Int`.

mod common;

use common::{PACKAGE, typecheck};
use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_ir::lower_program;
use koja_ir_eval::{Interpreter, Value};
use koja_parser::ParseMode;

/// Entry whose synchronous `run` encodes the argv check in its exit
/// code: 0 when the config is exactly `["alpha", "beta"]`, 1
/// otherwise.
const ARGV_ENTRY: &str = r#"
    struct ArgvEntry
      args: List<String>
    end

    impl Process<List<String>, (), ()> for ArgvEntry
      fn start(config: List<String>) -> Result<Self, StopReason>
        Result.Ok(ArgvEntry{args: config})
      end

      fn handle(self, msg: (), from: Option<ReplyTo<()>>) -> Step<Self>
        Step.Continue(self)
      end

      fn run(self) -> StopReason
        if self.args.get(0) == Option.Some("alpha") and self.args.get(1) == Option.Some("beta")
          StopReason.Normal
        else
          StopReason.Shutdown
        end
      end
    end
    "#;

fn run_argv_entry(args: &[&str]) -> Value {
    let checked = typecheck(&dedent(ARGV_ENTRY), ParseMode::File);
    let entry = Identifier::new(PACKAGE, vec!["ArgvEntry".to_string()]);
    let program = lower_program(&checked, &entry).expect("lowering should succeed");
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    Interpreter::run_program(&program, &args).expect("entry should run to completion")
}

#[test]
fn argv_config_materializes_from_args() {
    assert_eq!(run_argv_entry(&["alpha", "beta"]), Value::Int(0));
}

#[test]
fn argv_config_defaults_to_empty_without_args() {
    assert_eq!(run_argv_entry(&[]), Value::Int(1));
}

#[test]
fn argv_config_order_is_preserved() {
    assert_eq!(run_argv_entry(&["beta", "alpha"]), Value::Int(1));
}
