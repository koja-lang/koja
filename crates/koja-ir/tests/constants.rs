//! Pooled package constants lower to [`IRInstruction::LoadConst`] once
//! per reference site while the package pool holds a single entry per
//! `const` declaration.

use koja_ast::util::dedent;

use koja_ir::{IRInstruction, IRScript};

mod common;

use common::{PACKAGE, lower_script_source};

/// Counts `LoadConst` instructions reachable from the test package —
/// both the script body and any user-package helper fns. Stdlib
/// autoimport packages (e.g. `Global.io`'s `STDIN`/`STDOUT`/`STDERR`
/// struct constants) emit their own `LoadConst`s on field access —
/// those would inflate the count and obscure what these tests are
/// actually asserting about user-package lowering.
fn count_load_const(script: &IRScript) -> usize {
    let is_load_const = |inst: &&IRInstruction| matches!(inst, IRInstruction::LoadConst { .. });
    let in_body = script
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
        .filter(is_load_const)
        .count();
    let in_fns = script
        .packages
        .iter()
        .filter(|p| p.package == PACKAGE)
        .flat_map(|p| p.functions.values())
        .flat_map(|function| function.blocks.iter())
        .flat_map(|block| block.instructions.iter())
        .filter(is_load_const)
        .count();
    in_body + in_fns
}

/// Pooled-constant count for the test package only — same scoping
/// rationale as [`count_load_const`].
fn pooled_constants_len(script: &IRScript) -> usize {
    script
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

        ORIGIN.x + ORIGIN.y
        ";

    let script = lower_script_source(&dedent(source));
    assert_eq!(
        pooled_constants_len(&script),
        1,
        "expected one pooled struct constant, constants={:?}",
        script
            .packages
            .iter()
            .find(|p| p.package == PACKAGE)
            .map(|p| &p.constants)
    );
    assert_eq!(
        count_load_const(&script),
        2,
        "each field read should load the pooled constant",
    );
}

#[test]
fn primitive_constant_does_not_pool_or_emit_load_const() {
    let source = "
        const K = 99

        K + K
        ";

    let script = lower_script_source(&dedent(source));
    assert_eq!(pooled_constants_len(&script), 0);
    assert_eq!(count_load_const(&script), 0);
}
