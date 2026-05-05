//! IR lowering coverage for `if` and `unless` statements.
//!
//! Pins the basic-block CFG shape: an `if` lowers to a
//! `CondBranch(then, merge)` terminator on the entry block, a
//! body-bearing then-block ending in `Branch(merge)`, and a merge
//! block holding the if-expression's `Const::Unit` result. `unless`
//! mirrors the shape with the arms swapped on the `CondBranch`.

use std::path::PathBuf;

use expo_alpha_ir::{
    ConstValue, IRFunction, IRInstruction, IRProgram, IRTerminator, IRType, lower_program,
};
use expo_alpha_typecheck::check_program;
use expo_ast::identifier::Identifier;
use expo_ast::util::dedent;
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

fn lower(source: &str) -> IRProgram {
    let parsed = parse_program(
        vec![SourceFile {
            package: PACKAGE.to_string(),
            path: PathBuf::from("branches.expo"),
            source: source.to_string(),
        }],
        ParseMode::File,
    );
    let checked = check_program(parsed).unwrap_or_else(|f| panic!("alpha typecheck failed:\n{f}"));
    let entry = Identifier::new(PACKAGE, vec!["main".to_string()]);
    lower_program(&checked, entry).expect("lowering should succeed")
}

fn function<'a>(program: &'a IRProgram, name: &str) -> &'a IRFunction {
    let mangled = format!("{PACKAGE}.{name}");
    program
        .function(&mangled)
        .unwrap_or_else(|| panic!("missing function `{mangled}` in IRProgram"))
}

#[test]
fn if_lowers_to_three_blocks_with_cond_branch_terminator() {
    let source = "
        fn cond_true -> Bool
          true
        end

        fn main
          if cond_true()
            1
          end
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    assert_eq!(
        main.blocks.len(),
        3,
        "expected entry/then/merge blocks; got {} blocks",
        main.blocks.len(),
    );

    let entry = &main.blocks[0];
    let IRTerminator::CondBranch {
        cond: _,
        then_block,
        else_block,
    } = entry.terminator
    else {
        panic!(
            "expected entry to terminate in CondBranch; got {:?}",
            entry.terminator
        );
    };

    // The then-block in an if-no-else is the body block; the
    // else-target is the merge block (cond=false falls through).
    let then = main
        .blocks
        .iter()
        .find(|b| b.id == then_block)
        .expect("then-block missing");
    let merge = main
        .blocks
        .iter()
        .find(|b| b.id == else_block)
        .expect("merge-block missing");
    assert_eq!(
        then.terminator,
        IRTerminator::Branch(merge.id),
        "then-block should branch into the merge block",
    );

    // Merge holds the if-expression's Unit result and the function's
    // trailing Return.
    let unit_inst = merge
        .instructions
        .iter()
        .find(|i| {
            matches!(
                i,
                IRInstruction::Const {
                    value: ConstValue::Unit,
                    ..
                }
            )
        })
        .expect("merge should hold a Const::Unit");
    let dest = unit_inst.dest();
    assert_eq!(
        merge.terminator,
        IRTerminator::Return { value: Some(dest) },
        "merge block should `Return` the Unit value as the if-expression's result",
    );
}

#[test]
fn unless_swaps_then_and_else_relative_to_if() {
    // Same call-based condition as the `if` test; the only IR
    // difference between `if` and `unless` is which arm of the
    // CondBranch carries the body block.
    let source = "
        fn cond_false -> Bool
          false
        end

        fn main
          unless cond_false()
            1
          end
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    let entry = &main.blocks[0];
    let IRTerminator::CondBranch {
        cond: _,
        then_block,
        else_block,
    } = entry.terminator
    else {
        panic!(
            "expected entry to terminate in CondBranch; got {:?}",
            entry.terminator
        );
    };

    // For `unless`, cond=true skips to merge (then_block) and
    // cond=false runs the body (else_block).
    let then_target = main
        .blocks
        .iter()
        .find(|b| b.id == then_block)
        .expect("then-target block missing");
    let body = main
        .blocks
        .iter()
        .find(|b| b.id == else_block)
        .expect("body block missing");
    assert_eq!(
        body.terminator,
        IRTerminator::Branch(then_target.id),
        "unless body should jump into the merge block",
    );
    assert_eq!(
        then_target.label, "unless_merge",
        "then-arm of unless should target the merge block",
    );
}

#[test]
fn if_function_returns_unit() {
    let source = "
        fn cond_true -> Bool
          true
        end

        fn main
          if cond_true()
            1
          end
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    assert_eq!(
        main.return_type,
        IRType::Unit,
        "if without else is Unit-typed",
    );
}

#[test]
fn early_return_inside_if_closes_then_branch() {
    // Identifier references in function bodies aren't resolved
    // until the locals/parameters slice; the cond is therefore a
    // literal `true` rather than a parameter. The shape under
    // test (early `return` in the then-arm + trailing fall-through
    // in the merge) is independent of where the cond comes from.
    let source = "
        fn pick -> Int
          if true
            return 1
          end
          2
        end

        fn main
          pick()
        end
        ";

    let program = lower(&dedent(source));
    let pick = function(&program, "pick");
    // pick has at least three blocks (entry, if_then, if_merge);
    // if_then ends in `Return Some(_)` and if_merge ends in
    // `Return Some(_)` for the trailing `2`.
    let return_blocks: Vec<_> = pick
        .blocks
        .iter()
        .filter(|b| matches!(b.terminator, IRTerminator::Return { value: Some(_) }))
        .collect();
    assert_eq!(
        return_blocks.len(),
        2,
        "expected two Return-terminated blocks (then-branch's early return + merge fall-through); \
         got {return_blocks:#?}",
    );
}
