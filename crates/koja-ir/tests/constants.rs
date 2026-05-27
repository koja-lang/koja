//! Pooled package constants lower to [`IRInstruction::LoadConst`] once
//! per reference site while the package pool holds a single entry per
//! `const` declaration.

use koja_ast::util::dedent;

use koja_ir::{IRInstruction, IRProgram};

mod common;

use common::{PACKAGE, lower_program_source};

/// Counts `LoadConst` instructions in the test package only.
/// Stdlib autoimport packages (e.g. `Global.io`'s `STDIN`/`STDOUT`/
/// `STDERR` struct constants) emit their own `LoadConst`s on field
/// access — those would inflate the count and obscure what these
/// tests are actually asserting about user-package lowering.
fn count_load_const(program: &IRProgram) -> usize {
    let mut count = 0;
    for pkg in program.packages.iter().filter(|p| p.package == PACKAGE) {
        for function in pkg.functions.values() {
            for block in &function.blocks {
                count += block
                    .instructions
                    .iter()
                    .filter(|inst| matches!(inst, IRInstruction::LoadConst { .. }))
                    .count();
            }
        }
    }
    count
}

/// Pooled-constant count for the test package only — same scoping
/// rationale as [`count_load_const`].
fn pooled_constants_len(program: &IRProgram) -> usize {
    program
        .packages
        .iter()
        .filter(|p| p.package == PACKAGE)
        .map(|p| p.constants.len())
        .sum()
}

#[test]
fn struct_constant_pools_once_and_emits_load_const_per_field_read() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        const ORIGIN = Point{x: 10, y: 32}

        fn main
          ORIGIN.x + ORIGIN.y
        end
        ";

    let program = lower_program_source(&dedent(source));
    assert_eq!(
        pooled_constants_len(&program),
        1,
        "expected one pooled struct constant, constants={:?}",
        program
            .packages
            .iter()
            .find(|p| p.package == PACKAGE)
            .map(|p| &p.constants)
    );
    assert_eq!(
        count_load_const(&program),
        2,
        "each field read should load the pooled constant",
    );
}

#[test]
fn primitive_constant_does_not_pool_or_emit_load_const() {
    let source = "
        const K = 99

        fn main
          K + K
        end
        ";

    let program = lower_program_source(&dedent(source));
    assert_eq!(pooled_constants_len(&program), 0);
    assert_eq!(count_load_const(&program), 0);
}
