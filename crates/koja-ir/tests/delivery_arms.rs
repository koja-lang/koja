use koja_ast::identifier::Identifier;
use koja_ir::{IRInstruction, ReceiveTag, lower_program};
use koja_parser::ParseMode;

mod common;

use common::{PACKAGE, all_instructions, typecheck};

fn lower_entry(source: &str) -> koja_ir::IRProgram {
    let checked = typecheck(source, ParseMode::File);
    let entry = Identifier::new(PACKAGE, vec!["App".to_string()]);
    lower_program(&checked, &entry).expect("lowering should succeed")
}

fn has_delivery_arm(program: &koja_ir::IRProgram, tag: ReceiveTag) -> bool {
    program
        .packages
        .iter()
        .flat_map(|package| package.functions.values())
        .flat_map(|function| all_instructions(&function.blocks))
        .any(|instruction| {
            matches!(instruction, IRInstruction::Receive { arms, .. } if arms.iter().any(|arm| arm.tag == tag))
        })
}

#[test]
fn exit_signal_message_adds_delivery_arm() {
    let program = lower_entry(
        "
        alias Process.Step
        alias Process.StopReason

        struct App
        end

        impl Process<App, Process.ExitSignal, ()> for App
          fn start(config: App) -> Result<Self, StopReason>
            Result.Ok(config)
          end

          fn handle(self, msg: Process.ExitSignal, from: Option<ReplyTo<()>>) -> Step<Self>
            Step.Continue(self)
          end
        end
        ",
    );

    assert!(has_delivery_arm(&program, ReceiveTag::ExitSignal));
}

#[test]
fn io_ready_union_message_adds_delivery_arm() {
    let program = lower_entry(
        "
        alias IO.Ready as IOReady
        alias Process.Step
        alias Process.StopReason

        struct App
        end

        enum AppMsg
          Tick
        end

        impl Process<App, AppMsg | IOReady, ()> for App
          fn start(config: App) -> Result<Self, StopReason>
            Result.Ok(config)
          end

          fn handle(self, msg: AppMsg | IOReady, from: Option<ReplyTo<()>>) -> Step<Self>
            Step.Continue(self)
          end
        end
        ",
    );

    assert!(has_delivery_arm(&program, ReceiveTag::IOReady));
}
