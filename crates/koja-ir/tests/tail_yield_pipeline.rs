use koja_ir::{IRFunction, IRInstruction, IRTerminator};

mod common;

use common::{function, lower_program_source as lower};

fn has_tail_call_to(function: &IRFunction, suffix: &str) -> bool {
    function.blocks.iter().any(|block| {
        matches!(
            block.terminator,
            IRTerminator::TailCall { ref callee, .. } if callee.mangled().ends_with(suffix)
        )
    })
}

/// `try self(...)` in tail position of a `! E` function forwards the
/// callee's `Result` unchanged at typecheck, so the lowered shape is a
/// plain call-then-return and loopifies. The list accumulator reads a
/// slot whose exit drop sits in the same block, so it moves through the
/// back-edge with no `Clone`.
#[test]
fn try_forwarded_self_call_gains_tail_terminator_without_cloning_the_accumulator() {
    let program = lower(
        "
        fn build(n: Int, acc: List<Int>) -> List<Int> ! String
          if n == 0
            return acc
          end

          acc = acc.append(n)
          try build(n - 1, acc)
        end
        ",
    );
    let build = function(&program, "build");

    assert!(
        has_tail_call_to(build, ".build/2"),
        "expected a TailCall; blocks: {:#?}",
        build.blocks,
    );
    let tail_block = build
        .blocks
        .iter()
        .find(|block| matches!(block.terminator, IRTerminator::TailCall { .. }))
        .unwrap();
    let clones = tail_block
        .instructions
        .iter()
        .any(|instruction| match instruction {
            IRInstruction::Clone { .. } => true,
            IRInstruction::Call { callee, .. } => callee.mangled().contains(".$clone$"),
            _ => false,
        });
    assert!(
        !clones,
        "the accumulator must move, not clone: {:#?}",
        tail_block.instructions,
    );
}

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
                if callee.mangled().ends_with(".countdown/1")
        )
    }));
    assert!(countdown.blocks.iter().any(|block| {
        block
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, IRInstruction::YieldCheck))
    }));
}
