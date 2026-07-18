use koja_ir::{FunctionKind, IRInstruction};

mod common;

use common::{all_instructions, lower_program_source as lower};

#[test]
fn spawn_config_deep_copy_registers_glue() {
    let program = lower(
        "
        alias Process.Step
        alias Process.StopReason

        struct Config
          text: String
        end

        struct Worker
        end

        impl Process<Config, (), ()> for Worker
          fn start(config: Config) -> Result<Self, StopReason>
            Result.Ok(Worker{})
          end

          fn handle(self, msg: (), from: Option<ReplyTo<()>>) -> Step<Self>
            Step.Continue(self)
          end
        end

        fn launch -> Ref<(), ()>
          spawn Worker.start(Config{text: \"hello\"})
        end
        ",
    );
    program
        .packages
        .iter()
        .flat_map(|package| package.functions.values())
        .flat_map(|function| all_instructions(&function.blocks))
        .find(|instruction| matches!(instruction, IRInstruction::DeepCopy { .. }))
        .expect("spawn config should be deep-copied");
    assert!(program.packages.iter().any(|package| {
        package
            .functions
            .values()
            .any(|function| matches!(function.kind, FunctionKind::DeepCopyGlue))
    }));
}
