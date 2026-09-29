//! Pooled package constants lower to [`IRInstruction::LoadConst`] once
//! per reference site while the package pool holds a single entry per
//! `const` declaration. Values the static folder cannot express pool
//! as [`IRConstantValue::Built`] with one synthesized init function,
//! and `built_constant_order` runs each init after the constants it
//! reaches.

use koja_ir::{ConstValue, IRConstantValue, IRInstruction, IRScript, IRType, lower_script};
use koja_parser::ParseMode;

mod common;

use common::{
    PACKAGE, all_instructions, expect_diagnostics, lower_script_source, script_function,
    script_function_names, typecheck,
};

/// The pool entry for the test package constant `name`.
fn pooled_value<'a>(script: &'a IRScript, name: &str) -> &'a IRConstantValue {
    let mangled = format!("{PACKAGE}.{name}");
    script
        .constant_value(&mangled)
        .unwrap_or_else(|| panic!("constant `{mangled}` is not pooled"))
}

/// Assert the constant `name` is `Built` and return its init symbol.
fn built_init(script: &IRScript, name: &str) -> String {
    match pooled_value(script, name) {
        IRConstantValue::Built { init, .. } => init.mangled().to_string(),
        other => panic!("expected `{name}` to be a built constant, got {other:?}"),
    }
}

/// The test package's `built_constant_order` as bare constant names.
fn built_order(script: &IRScript) -> Vec<String> {
    let prefix = format!("{PACKAGE}.");
    script
        .built_constant_order
        .iter()
        .map(|symbol| {
            symbol
                .mangled()
                .strip_prefix(&prefix)
                .unwrap_or_else(|| panic!("unexpected constant `{symbol}` in the order"))
                .to_string()
        })
        .collect()
}

/// Lower a script and return its lowering diagnostics.
fn lower_script_diagnostics(source: &str) -> Vec<String> {
    let checked = typecheck(source, ParseMode::Script);
    let err = lower_script(&checked).expect_err("script lowering should surface diagnostics");
    expect_diagnostics(err)
}

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

// Built constants. Anything the static folder cannot express pools
// as `Built` with a synthesized `<symbol>__init` function, and
// every read is a `LoadConst` against that entry.

#[test]
fn list_constant_builds_once_and_loads_per_read() {
    let source = "
        const PRIMES = [2, 3, 5]

        fn first() -> Int
          PRIMES.length()
        end

        PRIMES.length() + first()
        ";

    let script = lower_script_source(source);
    assert_eq!(pooled_constants_len(&script), 1);
    let IRConstantValue::Built { init, ty } = pooled_value(&script, "PRIMES") else {
        panic!(
            "expected a built list constant, got {:?}",
            pooled_value(&script, "PRIMES")
        );
    };
    assert_eq!(init.mangled(), "TestApp.PRIMES__init");
    assert_eq!(*ty, IRType::List(Box::new(IRType::Int64)));

    let init_fn = script_function(&script, "PRIMES__init");
    assert!(init_fn.params.is_empty(), "init takes no parameters");
    assert_eq!(init_fn.return_type, *ty);
    let inits = script_function_names(&script)
        .into_iter()
        .filter(|name| name.ends_with("__init"))
        .count();
    assert_eq!(inits, 1, "two reads share one init");

    assert_eq!(count_load_const(&script), 2);
    assert_eq!(built_order(&script), ["PRIMES"]);
}

#[test]
fn map_and_struct_with_list_constants_build() {
    let source = "
        struct Config
          name: String
          ports: List<Int>
        end

        const PORTS = [\"http\": 80, \"https\": 443]
        const DEFAULT = Config{name: \"web\", ports: [80, 443]}

        PORTS.length() + DEFAULT.ports.length()
        ";

    let script = lower_script_source(source);
    assert_eq!(built_init(&script, "PORTS"), "TestApp.PORTS__init");
    assert_eq!(built_init(&script, "DEFAULT"), "TestApp.DEFAULT__init");
    assert_eq!(built_order(&script), ["DEFAULT", "PORTS"]);
}

#[test]
fn set_constant_builds_through_the_carrier_rewrite() {
    let source = "
        const TAGS: Set<String> = [\"a\", \"b\"]

        TAGS.length()
        ";

    let script = lower_script_source(source);
    let IRConstantValue::Built { ty, .. } = pooled_value(&script, "TAGS") else {
        panic!("expected a built set constant");
    };
    assert_eq!(*ty, IRType::Set(Box::new(IRType::String)));
}

#[test]
fn payload_variant_and_generic_struct_constants_build() {
    // The folder only expresses unit variants and non-generic
    // structs. Both of these would fold to a wrong value, so they
    // build at start instead.
    let source = "
        enum Shape
          Dot
          Circle(Float)
        end

        struct Pair<T>
          a: T
          b: T
        end

        const UNIT = Shape.Circle(1.0)
        const ONES = Pair{a: 1, b: 1}
        const DOT = Shape.Dot

        ONES.a
        ";

    let script = lower_script_source(source);
    assert_eq!(built_init(&script, "UNIT"), "TestApp.UNIT__init");
    assert_eq!(built_init(&script, "ONES"), "TestApp.ONES__init");
    assert!(
        matches!(
            pooled_value(&script, "DOT"),
            IRConstantValue::EnumVariant { .. }
        ),
        "a unit variant still folds statically",
    );
}

#[test]
fn string_constant_keeps_its_static_shape() {
    let source = "
        const GREETING = \"hi\"

        GREETING.length()
        ";

    let script = lower_script_source(source);
    assert_eq!(
        pooled_value(&script, "GREETING"),
        &IRConstantValue::Primitive(ConstValue::String("hi".to_string()))
    );
    assert!(built_order(&script).is_empty());
}

#[test]
fn constant_read_of_a_scalar_constant_folds_inline() {
    let source = "
        const ALIAS = BASE
        const BASE = 7

        ALIAS + BASE
        ";

    let script = lower_script_source(source);
    assert_eq!(pooled_constants_len(&script), 0);
    assert_eq!(count_load_const(&script), 0);
}

// Startup order. `IRPackage::constants` is a `BTreeMap`, so symbol
// order is alphabetical, and these tests name the dependent so it
// sorts first.

#[test]
fn built_constant_read_orders_the_dependency_first() {
    let source = "
        const ALL = [BASE]
        const BASE = [1, 2]

        ALL.length()
        ";

    let script = lower_script_source(source);
    assert_eq!(built_order(&script), ["BASE", "ALL"]);
    let init_fn = script_function(&script, "ALL__init");
    let loads = all_instructions(&init_fn.blocks)
        .filter(|inst| matches!(inst, IRInstruction::LoadConst { .. }))
        .count();
    assert_eq!(loads, 1, "the init reads `BASE` through the pool");
}

#[test]
fn read_through_a_carrier_body_orders_the_dependency_first() {
    let source = "
        struct Roster
          names: List<String>
        end

        impl ListLiteral<String> for Roster
          fn from_list(list: List<String>) -> Self
            Roster{names: NAMES}
          end
        end

        const ALL: Roster = [\"x\"]
        const NAMES = [\"a\", \"b\"]

        ALL.names.length()
        ";

    let script = lower_script_source(source);
    assert_eq!(built_order(&script), ["NAMES", "ALL"]);
}

#[test]
fn startup_cycle_through_a_carrier_body_diagnoses() {
    let source = "
        struct Roster
          names: List<String>
        end

        impl ListLiteral<String> for Roster
          fn from_list(list: List<String>) -> Self
            STAFF
          end
        end

        const STAFF: Roster = [\"x\"]

        STAFF.names.length()
        ";

    let messages = lower_script_diagnostics(source);
    assert_eq!(messages.len(), 1, "got {messages:#?}");
    assert!(
        messages[0].starts_with("constant `TestApp.STAFF` depends on itself at startup through `")
            && messages[0].contains("Roster.from_list"),
        "got {}",
        messages[0]
    );
}
