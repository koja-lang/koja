//! Coverage for hub-only numeric widening across the IR pipeline:
//! a sized-numeric value flowing into a hub-typed slot stamps
//! `Coercion::NumericWiden` at typecheck and lowers to a single
//! [`IRInstruction::NumericWiden`] carrying the source and target
//! [`IRType`]s, from which the backends derive sign- vs
//! zero-extension.

use koja_ast::util::dedent;
use koja_ir::{IRFunction, IRInstruction, IRType};

mod common;

use common::{lower_script_source as lower, script_function};

/// Collect every `NumericWiden`'s `(from, to)` pair across the
/// function's blocks.
fn widen_pairs(function: &IRFunction) -> Vec<(&IRType, &IRType)> {
    function
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .filter_map(|i| match i {
            IRInstruction::NumericWiden { from, to, .. } => Some((from, to)),
            _ => None,
        })
        .collect()
}

#[test]
fn int32_arg_into_int_param_lowers_to_numeric_widen() {
    let source = "
        fn want_int(n: Int) -> Int
          n
        end

        fn caller(small: Int32) -> Int
          want_int(small)
        end

        caller(7)
        ";
    let script = lower(&dedent(source));
    let widens = widen_pairs(script_function(&script, "caller"));
    assert_eq!(
        widens,
        vec![(&IRType::Int32, &IRType::Int64)],
        "expected exactly one Int32 -> Int64 NumericWiden for the arg site",
    );
}

#[test]
fn float32_return_into_float_lowers_to_numeric_widen() {
    let source = "
        fn promote(f: Float32) -> Float
          f
        end

        promote(1.5)
        ";
    let script = lower(&dedent(source));
    let widens = widen_pairs(script_function(&script, "promote"));
    assert_eq!(
        widens,
        vec![(&IRType::Float32, &IRType::Float64)],
        "expected exactly one Float32 -> Float64 NumericWiden at the return site",
    );
}
