use koja_ir::{IRInstruction, IRTerminator};

mod common;

use common::{function, lower_program_source as lower};

#[test]
fn self_tail_call_gains_tail_terminator_and_yield_check() {
    let program = lower(
        "
        fn countdown(n: Int) -> Int
          if n == 0
            0
          else
            countdown(n - 1)
          end
        end
        ",
    );
    let countdown = function(&program, "countdown");

    assert!(countdown.blocks.iter().any(|block| {
        matches!(
            block.terminator,
            IRTerminator::TailCall { ref callee, .. }
                if callee.mangled().ends_with(".countdown")
        )
    }));
    assert!(countdown.blocks.iter().any(|block| {
        block
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, IRInstruction::YieldCheck))
    }));
}
