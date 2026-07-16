//! Pooled package constants lower to [`IRInstruction::LoadConst`] once
//! per reference site while the package pool holds a single entry per
//! `const` declaration.

use koja_ir::{ConstValue, IRConstantValue, IRInstruction, IRScript};

mod common;

use common::{PACKAGE, all_instructions, lower_script_source};

/// The test package's pooled constant values, in pool order.
fn pooled_values(script: &IRScript) -> Vec<&IRConstantValue> {
    script
        .packages
        .iter()
        .filter(|p| p.package == PACKAGE)
        .flat_map(|p| p.constants.values())
        .collect()
}

/// Counts `LoadConst` instructions reachable from the test package,
/// covering both the script body and any user-package helper fns.
/// Stdlib autoimport packages (e.g. `Global.io`'s `STDIN`/`STDOUT`/
/// `STDERR` struct constants) emit their own `LoadConst`s on field
/// access. Those would inflate the count and obscure what these
/// tests are actually asserting about user-package lowering.
fn count_load_const(script: &IRScript) -> usize {
    let is_load_const = |inst: &&IRInstruction| matches!(inst, IRInstruction::LoadConst { .. });
    let in_body = all_instructions(&script.blocks)
        .filter(is_load_const)
        .count();
    let in_fns = script
        .packages
        .iter()
        .filter(|p| p.package == PACKAGE)
        .flat_map(|p| p.functions.values())
        .flat_map(|function| all_instructions(&function.blocks))
        .filter(is_load_const)
        .count();
    in_body + in_fns
}

/// Pooled-constant count for the test package only, with the same
/// scoping rationale as [`count_load_const`].
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

    let script = lower_script_source(source);
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

    let script = lower_script_source(source);
    assert_eq!(pooled_constants_len(&script), 0);
    assert_eq!(count_load_const(&script), 0);
}

#[test]
fn binary_literal_constant_folds_to_exact_bytes() {
    // Mixed widths, a little-endian segment, and a string segment
    // must fold byte-identically to a runtime construction.
    let source = "
        const FRAME: Binary = <<0x53::8, 258::16 little, \"hi\", 4::32>>

        FRAME.byte_size()
        ";

    let script = lower_script_source(source);
    let values = pooled_values(&script);
    assert_eq!(values.len(), 1, "expected one pooled binary constant");
    let IRConstantValue::Primitive(ConstValue::Binary(bytes)) = values[0] else {
        panic!("expected a folded ConstValue::Binary, got {:?}", values[0]);
    };
    assert_eq!(bytes, &[0x53, 0x02, 0x01, b'h', b'i', 0, 0, 0, 4]);
    assert_eq!(count_load_const(&script), 1);
}

#[test]
fn bits_constant_folds_with_bit_length() {
    // A non-byte-aligned total folds to Bits with MSB-first packing
    // (101 in the top three bits) and the exact bit length.
    let source = "
        const FLAGS: Bits = <<5::3>>

        FLAGS
        ";

    let script = lower_script_source(source);
    let values = pooled_values(&script);
    assert_eq!(values.len(), 1, "expected one pooled bits constant");
    let IRConstantValue::Primitive(ConstValue::Bits { bytes, bit_length }) = values[0] else {
        panic!("expected a folded ConstValue::Bits, got {:?}", values[0]);
    };
    assert_eq!(bytes, &[0b1010_0000]);
    assert_eq!(*bit_length, 3);
}

#[test]
fn struct_constant_with_binary_field_folds_the_field() {
    let source = "
        struct Frame
          header: Binary
          version: Int
        end

        const DEFAULT: Frame = Frame{header: <<0x53::8>>, version: 3}

        DEFAULT.version
        ";

    let script = lower_script_source(source);
    let values = pooled_values(&script);
    assert_eq!(values.len(), 1, "expected one pooled struct constant");
    let IRConstantValue::Struct { fields, .. } = values[0] else {
        panic!("expected a pooled struct constant, got {:?}", values[0]);
    };
    assert!(
        fields.iter().any(
            |f| matches!(f, IRConstantValue::Primitive(ConstValue::Binary(b)) if b == &[0x53])
        ),
        "expected a folded Binary field, got {fields:?}",
    );
}
