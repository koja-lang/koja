//! Lowering coverage for string literals: `ExprKind::String { parts:
//! [Literal] }` → `IRInstruction::Const { ConstValue::String }` with
//! return type `IRType::String`. Interpolation surfaces as a
//! feature-gap diagnostic.

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
            path: PathBuf::from("lower_strings.expo"),
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
fn string_literal_lowers_to_const_string() {
    let source = "
        fn main -> String
          \"hello\"
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    assert_eq!(main.return_type, IRType::String);

    let block = main.blocks.first().expect("main has at least one block");
    assert_eq!(block.instructions.len(), 1);

    let IRInstruction::Const { dest, value } = &block.instructions[0] else {
        panic!(
            "expected a Const instruction, got {:?}",
            block.instructions[0]
        );
    };
    let ConstValue::String(text) = value else {
        panic!("expected ConstValue::String, got {value:?}");
    };
    assert_eq!(text, "hello");

    assert_eq!(
        block.terminator,
        IRTerminator::Return { value: Some(*dest) },
    );
}

#[test]
fn empty_string_literal_lowers_to_empty_const_string() {
    let source = "
        fn main -> String
          \"\"
        end
        ";

    let program = lower(&dedent(source));
    let main = function(&program, "main");
    let block = main.blocks.first().expect("main has at least one block");

    let IRInstruction::Const { value, .. } = &block.instructions[0] else {
        panic!("expected a Const instruction");
    };
    let ConstValue::String(text) = value else {
        panic!("expected ConstValue::String, got {value:?}");
    };
    assert!(text.is_empty(), "expected empty string, got {text:?}");
}
