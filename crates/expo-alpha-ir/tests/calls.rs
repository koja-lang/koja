//! IR lowering coverage for `ExprKind::Call`.
//!
//! Exercises: zero-arg callee lowers to a single `Call`
//! instruction; an arg-taking callee's function gets `ValueId`s
//! allocated up front in `IRFunction.params`; nested calls chain
//! two `Call` instructions through a shared intermediate
//! `ValueId`.

use std::path::PathBuf;

use expo_alpha_ir::{IRFunction, IRInstruction, IRProgram, IRTerminator, lower_program};
use expo_alpha_typecheck::check_program;
use expo_ast::identifier::Identifier;
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

fn lower(source: &str) -> IRProgram {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("calls.expo"),
            source: source.to_string(),
        }],
        ParseMode::File,
    );
    let checked = check_program(parsed).unwrap_or_else(|f| panic!("alpha typecheck failed:\n{f}"));
    let entry = Identifier::new(PACKAGE, vec!["main".to_string()]);
    lower_program(&checked, entry).expect("lowering should succeed")
}

fn function<'a>(program: &'a IRProgram, name: &str) -> &'a IRFunction {
    program
        .function(&Identifier::new(PACKAGE, vec![name.to_string()]))
        .unwrap_or_else(|| panic!("missing function `{name}` in IRProgram"))
}

fn count_calls(function: &IRFunction) -> usize {
    function
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .filter(|i| matches!(i, IRInstruction::Call { .. }))
        .count()
}

#[test]
fn zero_arg_call_lowers_to_single_call_instruction() {
    let program = lower(
        "\
fn answer -> Int
  42
end

fn main
  answer()
end
",
    );

    let main = function(&program, "main");
    let block = main.blocks.first().expect("main has at least one block");
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
    assert_eq!(
        callee,
        &Identifier::new(PACKAGE, vec!["answer".to_string()])
    );

    assert_eq!(
        block.terminator,
        IRTerminator::Return { value: Some(*dest) },
    );

    // `answer` itself should have been lowered with zero params.
    let answer = function(&program, "answer");
    assert_eq!(answer.params.len(), 0);
}

#[test]
fn arg_taking_callee_allocates_param_value_ids_before_body() {
    let program = lower(
        "\
fn take(x: Int) -> Int
  7
end

fn main
  take(99)
end
",
    );

    let take = function(&program, "take");
    assert_eq!(take.params.len(), 1, "take has one declared param");
    // Params are the first ids allocated, so the body's const
    // instruction should land at the *next* id.
    let param_id = take.params[0];
    let body_const = take
        .blocks
        .first()
        .expect("take has one block")
        .instructions
        .first()
        .expect("take body has at least one instruction");
    assert!(
        body_const.dest().0 > param_id.0,
        "body-produced value ({}) should be allocated after param value ({})",
        body_const.dest(),
        param_id,
    );

    // The call site wires `99` into the Call's args.
    let main = function(&program, "main");
    let calls: Vec<_> = main
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .filter_map(|i| match i {
            IRInstruction::Call { args, .. } => Some(args.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 1, "main should emit exactly one Call");
    assert_eq!(calls[0].len(), 1, "call passes one arg");
}

#[test]
fn nested_calls_chain_through_value_ids() {
    let program = lower(
        "\
fn a -> Int
  1
end

fn b -> Int
  2
end

fn main
  a() + b()
end
",
    );

    let main = function(&program, "main");
    assert_eq!(
        count_calls(main),
        2,
        "main should emit one Call per nested callee",
    );

    let block = &main.blocks[0];
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
    assert_eq!(a_callee.last(), "a");
    assert_eq!(b_callee.last(), "b");
    assert_eq!(*lhs, *a_dest);
    assert_eq!(*rhs, *b_dest);
}

#[test]
fn returned_value_flows_through_call_terminator() {
    // Sanity that the Call's `dest` gets plumbed into the
    // terminator when the call is the trailing expression.
    let program = lower(
        "\
fn answer -> Int
  42
end

fn main
  answer()
end
",
    );
    let main = function(&program, "main");
    let block = &main.blocks[0];
    let Some(IRInstruction::Call { dest, .. }) = block.instructions.last() else {
        panic!(
            "main's last instruction should be Call; got {:?}",
            block.instructions.last(),
        );
    };
    assert_eq!(
        block.terminator,
        IRTerminator::Return { value: Some(*dest) },
    );
}
