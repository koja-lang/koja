//! Coverage for expression-level lowering in `src/lower/expr.rs`.
//!
//! Headline coverage today is `ExprKind::Call`. A zero-arg callee
//! lowers to a single `Call` instruction. An arg-taking callee's
//! function gets `ValueId`s allocated up front in
//! `IRFunction.params`. Nested calls chain two `Call` instructions
//! through a shared intermediate `ValueId`. Other [`ExprKind`]
//! variants get their dedicated test files (literals + ops in
//! `lower_ops.rs`, `if`/`unless` in `lower_control_flow.rs`).

use koja_ir::{IRBasicBlock, IRInstruction, IRTerminator, IRType};

mod common;

use common::{
    PACKAGE, all_instructions, entry_block, lower_script_source as lower, script_function,
};

fn count_calls(blocks: &[IRBasicBlock]) -> usize {
    all_instructions(blocks)
        .filter(|i| matches!(i, IRInstruction::Call { .. }))
        .count()
}

#[test]
fn zero_arg_call_lowers_to_single_call_instruction() {
    let source = "
        fn answer -> Int
          42
        end

        answer()
        ";

    let script = lower(source);
    let block = entry_block(&script.blocks);
    assert_eq!(
        block.instructions.len(),
        1,
        "expected one Call instruction; got {:?}",
        block.instructions,
    );

    let IRInstruction::Call { dest, callee, args } = &block.instructions[0] else {
        panic!(
            "expected a Call instruction; got {:?}",
            block.instructions[0]
        );
    };
    assert_eq!(args.len(), 0);
    assert_eq!(callee.mangled(), format!("{PACKAGE}.answer"));

    assert_eq!(
        block.terminator,
        IRTerminator::Return { value: Some(*dest) },
    );

    // `answer` itself should have been lowered with zero params.
    let answer = script_function(&script, "answer");
    assert_eq!(answer.params.len(), 0);
}

#[test]
fn arg_taking_callee_allocates_param_value_ids_before_body() {
    let source = "
        fn take(x: Int) -> Int
          7
        end

        take(99)
        ";

    let script = lower(source);
    let take = script_function(&script, "take");
    assert_eq!(take.params.len(), 1, "take has one declared param");
    // Params are the first ids allocated, so the body's const
    // instruction should land at the *next* id.
    let param = &take.params[0];
    assert_eq!(
        param.ty,
        IRType::Int64,
        "lowering should stamp the param's IRType from the lifted signature",
    );
    // The entry block emits `LocalDecl` + `LocalWrite` for param
    // promotion ahead of the body's const. Both produce no `dest`,
    // so we walk past them to find the first body-produced value.
    let body_dest = entry_block(&take.blocks)
        .instructions
        .iter()
        .find_map(|inst| inst.dest())
        .expect("take body has at least one value-producing instruction");
    assert!(
        body_dest.0 > param.id.0,
        "body-produced value ({body_dest}) should be allocated after param value ({})",
        param.id,
    );

    // The call site wires `99` into the Call's args.
    let calls: Vec<_> = all_instructions(&script.blocks)
        .filter_map(|i| match i {
            IRInstruction::Call { args, .. } => Some(args.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 1, "script body should emit exactly one Call");
    assert_eq!(calls[0].len(), 1, "call passes one arg");
}

#[test]
fn nested_calls_chain_through_value_ids() {
    let source = "
        fn a -> Int
          1
        end

        fn b -> Int
          2
        end

        a() + b()
        ";

    let script = lower(source);
    assert_eq!(
        count_calls(&script.blocks),
        2,
        "script body should emit one Call per nested callee",
    );

    let block = &script.blocks[0];
    // Expected sequence: Call(a), Call(b), BinaryOp(Add). The
    // BinaryOp's operands should be the two Call dests.
    let [call_a, call_b, binop] = block.instructions.as_slice() else {
        panic!(
            "expected 3 instructions (Call, Call, BinaryOp); got {:?}",
            block.instructions,
        );
    };
    let IRInstruction::Call {
        dest: a_dest,
        callee: a_callee,
        ..
    } = call_a
    else {
        panic!("instruction 0 should be Call; got {call_a:?}");
    };
    let IRInstruction::Call {
        dest: b_dest,
        callee: b_callee,
        ..
    } = call_b
    else {
        panic!("instruction 1 should be Call; got {call_b:?}");
    };
    let IRInstruction::BinaryOp { lhs, rhs, .. } = binop else {
        panic!("instruction 2 should be BinaryOp; got {binop:?}");
    };
    assert_eq!(a_callee.mangled(), format!("{PACKAGE}.a"));
    assert_eq!(b_callee.mangled(), format!("{PACKAGE}.b"));
    assert_eq!(*lhs, *a_dest);
    assert_eq!(*rhs, *b_dest);
}

#[test]
fn returned_value_flows_through_call_terminator() {
    // Sanity that the Call's `dest` gets plumbed into the
    // terminator when the call is the trailing expression.
    let source = "
        fn answer -> Int
          42
        end

        answer()
        ";

    let script = lower(source);
    let block = &script.blocks[0];
    let Some(IRInstruction::Call { dest, .. }) = block.instructions.last() else {
        panic!(
            "script body's last instruction should be Call; got {:?}",
            block.instructions.last(),
        );
    };
    assert_eq!(
        block.terminator,
        IRTerminator::Return { value: Some(*dest) },
    );
}
