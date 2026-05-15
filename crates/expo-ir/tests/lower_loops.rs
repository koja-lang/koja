//! Coverage for `loop` and `while` lowering in `src/lower/loops.rs`.
//!
//! Pins the three-block CFG shape for `while`:
//!
//! - Open block branches unconditionally to `while_header`.
//! - Header lowers the condition and terminates with
//!   [`IRTerminator::CondBranch`] to body / exit.
//! - Body lowers its statements and emits a back-edge
//!   [`IRTerminator::Branch`] to the header.
//! - Exit emits a fresh `Const::Unit` and continues the surrounding
//!   flow.
//!
//! And the two-block CFG shape for `loop` (no header — there's no
//! condition):
//!
//! - Open block branches unconditionally to `loop_body`.
//! - Body emits a back-edge [`IRTerminator::Branch`] to itself.
//!   `break` inside the body closes its own flow with a
//!   `Branch(loop_exit)`.
//! - Exit emits a fresh `Const::Unit` (only reachable when at least
//!   one `break` fires).
//!
//! Loop-carried state lives in alloca slots
//! ([`IRInstruction::LocalRead`] / [`IRInstruction::LocalWrite`]) —
//! no block params on the header. Block-param SSA stays for the
//! `if`/`else` joins that may live inside the body, but the loop
//! itself doesn't introduce one.
//!
//! `for` lowers via the typecheck-side `synthesize::for_desugar`
//! into the same while + match shape; [`for_lowers_via_desugar_to_while_plus_match`]
//! pins the resulting IR end-to-end.

use expo_alpha_ir::{BranchTarget, IRInstruction, IRTerminator, IRType};
use expo_ast::util::dedent;

mod common;

use common::{function, lower_program_source as lower};

#[test]
fn while_lowers_to_header_body_exit_with_back_edge() {
    let source = "
        fn main
          i = 0
          while i < 3
            i = i + 1
          end
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");

    // entry, while_header, while_body, while_exit — 4 blocks.
    assert_eq!(
        main.blocks.len(),
        4,
        "expected entry/header/body/exit blocks; got {} blocks",
        main.blocks.len(),
    );

    let entry = &main.blocks[0];
    let IRTerminator::Branch(BranchTarget {
        block: header_id,
        args,
    }) = &entry.terminator
    else {
        panic!(
            "entry should branch unconditionally to header; got {:?}",
            entry.terminator,
        );
    };
    assert!(
        args.is_empty(),
        "entry branch carries no args; got {args:?}",
    );

    let header = main
        .blocks
        .iter()
        .find(|b| b.id == *header_id)
        .expect("header block missing");
    assert_eq!(
        header.label, "while_header",
        "expected header label `while_header`, got `{}`",
        header.label,
    );
    assert!(
        header.params.is_empty(),
        "header carries no block params (loop state lives in alloca slots); got {:?}",
        header.params,
    );

    let IRTerminator::CondBranch {
        cond: _,
        else_target,
        then_target,
    } = &header.terminator
    else {
        panic!(
            "header should terminate with CondBranch; got {:?}",
            header.terminator,
        );
    };

    let body = main
        .blocks
        .iter()
        .find(|b| b.id == then_target.block)
        .expect("body block missing");
    assert_eq!(body.label, "while_body");
    let IRTerminator::Branch(BranchTarget {
        block: back_edge_target,
        args: back_edge_args,
    }) = &body.terminator
    else {
        panic!(
            "body should terminate with back-edge Branch; got {:?}",
            body.terminator,
        );
    };
    assert_eq!(
        *back_edge_target, *header_id,
        "back-edge should target header block",
    );
    assert!(
        back_edge_args.is_empty(),
        "back-edge carries no args; got {back_edge_args:?}",
    );

    let exit = main
        .blocks
        .iter()
        .find(|b| b.id == else_target.block)
        .expect("exit block missing");
    assert_eq!(exit.label, "while_exit");
}

#[test]
fn while_exit_block_emits_unit_and_continues_flow() {
    // Surface expression types as Unit, so the body's trailing flow
    // through the exit block returns Unit; verify the exit block
    // contains a `Const::Unit` and that the function returns Unit.
    let source = "
        fn main
          i = 0
          while i < 1
            i = i + 1
          end
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    assert_eq!(main.return_type, IRType::Unit);

    let exit = main
        .blocks
        .iter()
        .find(|b| b.label == "while_exit")
        .expect("exit block missing");
    assert!(
        exit.instructions.iter().any(|i| matches!(
            i,
            IRInstruction::Const {
                value: expo_alpha_ir::ConstValue::Unit,
                ..
            }
        )),
        "exit block should emit a Const::Unit; got {:?}",
        exit.instructions,
    );
}

#[test]
fn while_body_writes_propagate_through_alloca_slots() {
    // The mutable `i` slot is read in the header (cond evaluation)
    // and re-read + written in the body, all through LocalRead /
    // LocalWrite against the same `IRLocalId` — no block params on
    // the header.
    let source = "
        fn main
          i = 0
          while i < 3
            i = i + 1
          end
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");

    let header = main
        .blocks
        .iter()
        .find(|b| b.label == "while_header")
        .expect("header missing");
    let header_local_reads: Vec<_> = header
        .instructions
        .iter()
        .filter(|i| matches!(i, IRInstruction::LocalRead { .. }))
        .collect();
    assert!(
        !header_local_reads.is_empty(),
        "header should LocalRead the loop counter for the cond; got {:?}",
        header.instructions,
    );

    let body = main
        .blocks
        .iter()
        .find(|b| b.label == "while_body")
        .expect("body missing");
    let body_writes: Vec<_> = body
        .instructions
        .iter()
        .filter(|i| matches!(i, IRInstruction::LocalWrite { .. }))
        .collect();
    assert!(
        !body_writes.is_empty(),
        "body should LocalWrite the loop counter; got {:?}",
        body.instructions,
    );
}

#[test]
fn while_with_return_inside_body_terminates_function() {
    // An early `return` inside the loop body closes its own flow.
    // The body block's terminator should be a Return, not a
    // back-edge Branch.
    let source = "
        fn main -> Int
          i = 0
          while i < 10
            return i
          end
          i
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");

    let body = main
        .blocks
        .iter()
        .find(|b| b.label == "while_body")
        .expect("body missing");
    assert!(
        matches!(body.terminator, IRTerminator::Return { .. }),
        "early-return body should terminate with Return; got {:?}",
        body.terminator,
    );
}

#[test]
fn for_lowers_via_desugar_to_while_plus_match() {
    // `for x in c` desugars to a while loop wrapping a match on
    // `c.get(idx)`; we should see calls to `Counter.length` /
    // `Counter.get` and a back-edge to the header from the body's
    // descendant blocks.
    let source = "
        struct Counter
          start: Int
          finish: Int
        end

        impl Counter
          fn length(self) -> Int
            self.finish - self.start
          end

          fn get(self, index: Int) -> Option<Int>
            Option.Some(self.start + index)
          end
        end

        fn main
          c = Counter{start: 0, finish: 3}
          sum = 0
          for x in c
            sum = sum + x
          end
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");

    let header = main
        .blocks
        .iter()
        .find(|b| b.label == "while_header")
        .expect("for-desugar should produce a while_header block");
    assert!(
        matches!(header.terminator, IRTerminator::CondBranch { .. }),
        "while_header should terminate with CondBranch; got {:?}",
        header.terminator,
    );

    let body = main
        .blocks
        .iter()
        .find(|b| b.label == "while_body")
        .expect("for-desugar should produce a while_body block");
    let exit = main
        .blocks
        .iter()
        .find(|b| b.label == "while_exit")
        .expect("for-desugar should produce a while_exit block");
    let _ = exit;

    let calls: Vec<&str> = main
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .filter_map(|inst| match inst {
            IRInstruction::Call { callee, .. } => Some(callee.mangled()),
            _ => None,
        })
        .collect();
    assert!(
        calls.iter().any(|c| c.contains("Counter.length")),
        "for-desugar should call `Counter.length`; got calls {calls:?}",
    );
    assert!(
        calls.iter().any(|c| c.contains("Counter.get")),
        "for-desugar should call `Counter.get`; got calls {calls:?}",
    );

    // Don't pin the exact match-arm block layout — only that some
    // descendant block branches back to the header.
    let header_id = header.id;
    let has_back_edge = main.blocks.iter().any(|b| {
        b.id != body.id
            && b.id != header_id
            && matches!(
                &b.terminator,
                IRTerminator::Branch(BranchTarget { block, .. }) if *block == header_id,
            )
    });
    assert!(
        has_back_edge,
        "for-desugar should produce a back-edge to `while_header` from \
         a match-arm tail block; got blocks {:?}",
        main.blocks.iter().map(|b| &b.label).collect::<Vec<_>>(),
    );
}

#[test]
fn loop_lowers_to_body_with_self_back_edge() {
    // `loop end` (no break) emits an entry → loop_body branch and a
    // back-edge from loop_body to itself. The exit block is
    // synthesized but never reached at runtime — the IR still
    // contains it (every `loop` produces both blocks unconditionally).
    let source = "
        fn main
          loop
          end
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");

    let entry = &main.blocks[0];
    let IRTerminator::Branch(BranchTarget { block: body_id, .. }) = &entry.terminator else {
        panic!(
            "entry should branch unconditionally to loop_body; got {:?}",
            entry.terminator,
        );
    };

    let body = main
        .blocks
        .iter()
        .find(|b| b.id == *body_id)
        .expect("loop_body block missing");
    assert_eq!(body.label, "loop_body");
    let IRTerminator::Branch(BranchTarget {
        block: back_edge_target,
        ..
    }) = &body.terminator
    else {
        panic!(
            "loop_body should terminate with back-edge Branch; got {:?}",
            body.terminator,
        );
    };
    assert_eq!(*back_edge_target, *body_id, "back-edge must target body");

    main.blocks
        .iter()
        .find(|b| b.label == "loop_exit")
        .expect("loop_exit block missing (always synthesized, even if unreachable)");
}

#[test]
fn loop_with_break_lowers_break_as_branch_to_loop_exit() {
    // The body's `break` closes the body's flow with a
    // `Branch(loop_exit)`. The body block has no back-edge — its
    // terminator IS the break branch — and the exit block contains
    // a `Const::Unit`.
    let source = "
        fn main
          loop
            break
          end
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");

    let body = main
        .blocks
        .iter()
        .find(|b| b.label == "loop_body")
        .expect("loop_body missing");
    let exit = main
        .blocks
        .iter()
        .find(|b| b.label == "loop_exit")
        .expect("loop_exit missing");

    let IRTerminator::Branch(BranchTarget { block: target, .. }) = &body.terminator else {
        panic!(
            "body should terminate with break-branch; got {:?}",
            body.terminator,
        );
    };
    assert_eq!(
        *target, exit.id,
        "break should branch directly to loop_exit",
    );
    assert!(
        exit.instructions.iter().any(|i| matches!(
            i,
            IRInstruction::Const {
                value: expo_alpha_ir::ConstValue::Unit,
                ..
            }
        )),
        "loop_exit should emit Const::Unit; got {:?}",
        exit.instructions,
    );
}

#[test]
fn nested_break_targets_innermost_loop_exit() {
    // `loop loop break end end`: the inner `break` must branch to
    // the *inner* loop_exit, not the outer one. The IR shows two
    // pairs of (body, exit) blocks; the inner break-branch's target
    // is the inner exit (the second loop_exit by creation order).
    let source = "
        fn main
          loop
            loop
              break
            end
          end
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");

    let exit_blocks: Vec<_> = main
        .blocks
        .iter()
        .filter(|b| b.label == "loop_exit")
        .collect();
    assert_eq!(
        exit_blocks.len(),
        2,
        "expected two loop_exit blocks (one per nested loop); got {} (labels: {:?})",
        exit_blocks.len(),
        main.blocks.iter().map(|b| &b.label).collect::<Vec<_>>(),
    );
    // `lower_loop` for the inner loop runs nested inside the outer
    // — so by `fresh_block` order the outer's exit comes first
    // (created during the outer's `lower_loop` setup), then the
    // inner's exit. Bookkeeping mirrors v1's loop_exit stack push
    // order; pin the outer-then-inner shape.
    let outer_exit_id = exit_blocks[0].id;
    let inner_exit_id = exit_blocks[1].id;

    let body_blocks: Vec<_> = main
        .blocks
        .iter()
        .filter(|b| b.label == "loop_body")
        .collect();
    assert_eq!(body_blocks.len(), 2, "expected two loop_body blocks");

    // The inner body terminates with the break-branch to the inner
    // exit (it never falls through to its own back-edge because the
    // break is the only statement in the body).
    let inner_body = body_blocks
        .iter()
        .find(|b| {
            matches!(
                &b.terminator,
                IRTerminator::Branch(BranchTarget { block, .. }) if *block == inner_exit_id,
            )
        })
        .unwrap_or_else(|| {
            panic!(
                "expected one loop_body to break to inner_exit ({inner_exit_id}); \
                 got terminators {:?}",
                body_blocks
                    .iter()
                    .map(|b| &b.terminator)
                    .collect::<Vec<_>>(),
            )
        });
    assert!(
        !matches!(
            &inner_body.terminator,
            IRTerminator::Branch(BranchTarget { block, .. }) if *block == outer_exit_id,
        ),
        "inner break must NOT target outer exit; got {:?}",
        inner_body.terminator,
    );
}
