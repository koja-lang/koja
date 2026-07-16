//! Typecheck coverage for boolean and comparison operators
//! (`and`/`or`/`not`/`== != < > <= >=`). Pairs with
//! `pipeline::resolve::ops` in src. Mirrors `program_script.rs`: parse
//! and check a tiny script body, then inspect the trailing
//! expression's `resolution`. Error paths assert a diagnostic on
//! ill-typed programs.

mod common;

use common::{
    assert_script_fails_with, bool_type, float_type, global_leaf, int_type, trailing_resolution,
    typecheck_script as typecheck, typecheck_script_fail as typecheck_fail,
};

fn assert_trailing_is(source: &str, expected_name: &str) {
    let checked = typecheck(source);
    let expected = global_leaf(&checked, expected_name);
    let actual = trailing_resolution(&checked);
    assert_eq!(
        actual, expected,
        "source = {source:?} did not resolve to Global.{expected_name}",
    );
}

#[test]
fn logical_and_or_resolve_to_bool() {
    assert_trailing_is("true and false\n", "Bool");
    assert_trailing_is("true or false\n", "Bool");
}

#[test]
fn unary_not_resolves_to_bool() {
    assert_trailing_is("not true\n", "Bool");
}

#[test]
fn unary_neg_resolves_to_int() {
    assert_trailing_is("-7\n", "Int");
}

#[test]
fn comparisons_resolve_to_bool() {
    for source in [
        "1 == 1\n", "1 != 2\n", "1 < 2\n", "1 > 2\n", "1 <= 2\n", "1 >= 2\n",
    ] {
        let checked = typecheck(source);
        assert_eq!(
            trailing_resolution(&checked),
            bool_type(&checked),
            "source = {source:?}",
        );
    }
}

#[test]
fn bool_equality_is_allowed() {
    let checked = typecheck("true == false\n");
    assert_eq!(trailing_resolution(&checked), bool_type(&checked));
}

#[test]
fn int_type_helper_still_references_int() {
    // Sanity check that both `int_type` and `bool_type` correspond to
    // the stubs the resolver emits. Catches reverse-index breakage.
    let checked = typecheck("1 + 1\n");
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
    assert_ne!(int_type(&checked), bool_type(&checked));
}

#[test]
fn mixed_int_and_bool_and_diagnoses() {
    let failure = typecheck_fail("1 and true\n");
    assert_eq!(failure.diagnostics.len(), 1);
    assert!(
        failure.diagnostics[0].message.contains("`and`"),
        "unexpected diagnostic: {}",
        failure.diagnostics[0].message,
    );
}

#[test]
fn short_circuited_rhs_is_still_typechecked() {
    for source in ["false and 1\n", "true or 1\n"] {
        let failure = typecheck_fail(source);
        assert_eq!(failure.diagnostics.len(), 1, "source = {source:?}");
        assert!(
            failure.diagnostics[0].message.contains("Bool operands"),
            "unexpected diagnostic for {source:?}: {}",
            failure.diagnostics[0].message,
        );
    }
}

#[test]
fn ordering_on_bool_diagnoses() {
    let failure = typecheck_fail("true < false\n");
    assert_eq!(failure.diagnostics.len(), 1);
    assert!(
        failure.diagnostics[0].message.contains("Int or Float"),
        "unexpected diagnostic: {}",
        failure.diagnostics[0].message,
    );
}

#[test]
fn not_on_int_diagnoses() {
    let failure = typecheck_fail("not 1\n");
    assert_eq!(failure.diagnostics.len(), 1);
    assert!(
        failure.diagnostics[0].message.contains("Bool operand"),
        "unexpected diagnostic: {}",
        failure.diagnostics[0].message,
    );
}

#[test]
fn neg_on_bool_diagnoses() {
    let failure = typecheck_fail("-true\n");
    assert_eq!(failure.diagnostics.len(), 1);
    assert!(
        failure.diagnostics[0].message.contains("Int or Float"),
        "unexpected diagnostic: {}",
        failure.diagnostics[0].message,
    );
}

#[test]
fn string_concat_resolves_to_string() {
    assert_trailing_is("\"foo\" <> \"bar\"\n", "String");
}

#[test]
fn binary_concat_requires_binary_operands() {
    // Cross-type concat (String <> Binary) is rejected: the user
    // must convert through an explicit stdlib helper. Pin the
    // diagnostic shape so accidental cross-type acceptance gets
    // caught.
    assert_script_fails_with(
        "fn copy(b: Binary) -> Binary\n  \"hi\" <> b\nend\n\n1\n",
        &["String, Binary, or Bits"],
    );
}

#[test]
fn int_concat_diagnoses() {
    assert_script_fails_with("1 <> 2\n", &["String, Binary, or Bits"]);
}

#[test]
fn float_arithmetic_resolves_to_float() {
    for source in [
        "1.0 + 2.0\n",
        "1.0 - 2.0\n",
        "1.0 * 2.0\n",
        "1.0 / 2.0\n",
        "1.0 % 2.0\n",
    ] {
        let checked = typecheck(source);
        assert_eq!(
            trailing_resolution(&checked),
            float_type(&checked),
            "source = {source:?}",
        );
    }
}

#[test]
fn float_comparison_resolves_to_bool() {
    for source in ["1.0 < 2.0\n", "1.0 > 2.0\n", "1.0 <= 2.0\n", "1.0 >= 2.0\n"] {
        let checked = typecheck(source);
        assert_eq!(
            trailing_resolution(&checked),
            bool_type(&checked),
            "source = {source:?}",
        );
    }
}

#[test]
fn float_equality_resolves_to_bool() {
    let checked = typecheck("1.0 == 2.0\n");
    assert_eq!(trailing_resolution(&checked), bool_type(&checked));
}

#[test]
fn unary_neg_on_float_resolves_to_float() {
    let checked = typecheck("-3.14\n");
    assert_eq!(trailing_resolution(&checked), float_type(&checked));
}

#[test]
fn mixed_int_float_arith_diagnoses() {
    let failure = typecheck_fail("1 + 1.0\n");
    assert_eq!(failure.diagnostics.len(), 1);
    assert!(
        failure.diagnostics[0]
            .message
            .contains("Int, Float, or matching sized numeric"),
        "unexpected diagnostic: {}",
        failure.diagnostics[0].message,
    );
}

fn use_alias_int(extra: &str) -> String {
    let extern_decl = "@extern \"C\"\nfn produce_int64() -> Int64\n\n";
    format!("{extern_decl}result = produce_int64()\n{extra}\n")
}

fn use_alias_float(extra: &str) -> String {
    let extern_decl = "@extern \"C\"\nfn produce_float64() -> Float64\n\n";
    format!("{extern_decl}result = produce_float64()\n{extra}\n")
}

#[test]
fn int_alias_arith_resolves_to_int() {
    let checked = typecheck(&use_alias_int("result + 1"));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn int_alias_comparison_resolves_to_bool() {
    for source in [
        use_alias_int("result == 0"),
        use_alias_int("result != 0"),
        use_alias_int("result < 0"),
        use_alias_int("result >= 0"),
    ] {
        let checked = typecheck(&source);
        assert_eq!(
            trailing_resolution(&checked),
            bool_type(&checked),
            "source = {source:?}",
        );
    }
}

#[test]
fn float_alias_arith_resolves_to_float() {
    let checked = typecheck(&use_alias_float("result + 1.0"));
    assert_eq!(trailing_resolution(&checked), float_type(&checked));
}

#[test]
fn float_alias_comparison_resolves_to_bool() {
    let checked = typecheck(&use_alias_float("result >= 0.0"));
    assert_eq!(trailing_resolution(&checked), bool_type(&checked));
}

fn use_sized_pair(sized: &str, op_expr: &str) -> String {
    let extern_decl = format!("@extern \"C\"\nfn produce() -> {sized}\n\n");
    format!("{extern_decl}a = produce()\nb = produce()\n{op_expr}\n")
}

#[test]
fn same_sized_numeric_eq_resolves_to_bool() {
    for sized in [
        "Int8", "Int16", "Int32", "Int64", "UInt8", "UInt16", "UInt32", "UInt64", "Float32",
    ] {
        for op in ["==", "!=", "<", ">", "<=", ">="] {
            let source = use_sized_pair(sized, &format!("a {op} b"));
            let checked = typecheck(&source);
            assert_eq!(
                trailing_resolution(&checked),
                bool_type(&checked),
                "source = {source:?}",
            );
        }
    }
}

#[test]
fn cross_sized_numeric_eq_is_rejected() {
    let source = "@extern \"C\"\nfn produce_u8() -> UInt8\n\
        @extern \"C\"\nfn produce_i32() -> Int32\n\
        a = produce_u8()\n  b = produce_i32()\n  a == b\n";
    assert_script_fails_with(source, &["matching Bool, Float, Int, or String operands"]);
}

#[test]
fn same_sized_numeric_arith_resolves_to_operand_type() {
    // Int64 / Float64 take the alias path (collapse to Int / Float
    // via `types_equivalent`). Test the genuinely sized variants
    // here. `same_sized_numeric_eq_resolves_to_bool` above covers
    // the full set on the comparison side.
    for sized in [
        "Int8", "Int16", "Int32", "UInt8", "UInt16", "UInt32", "UInt64", "Float32",
    ] {
        for op in ["+", "-", "*", "/"] {
            let source = use_sized_pair(sized, &format!("a {op} b"));
            let checked = typecheck(&source);
            let expected = global_leaf(&checked, sized);
            assert_eq!(
                trailing_resolution(&checked),
                expected,
                "source = {source:?}",
            );
        }
    }
}

#[test]
fn sized_int_plus_int_literal_resolves_to_sized() {
    let source = "@extern \"C\"\nfn produce_i32() -> Int32\n\
        a = produce_i32()\n  a + 5\n";
    let checked = typecheck(source);
    let expected = global_leaf(&checked, "Int32");
    assert_eq!(trailing_resolution(&checked), expected);
}

#[test]
fn int_literal_plus_sized_int_resolves_to_sized() {
    let source = "@extern \"C\"\nfn produce_i32() -> Int32\n\
        a = produce_i32()\n  5 + a\n";
    let checked = typecheck(source);
    let expected = global_leaf(&checked, "Int32");
    assert_eq!(trailing_resolution(&checked), expected);
}

#[test]
fn cross_sized_numeric_arith_is_rejected() {
    let source = "@extern \"C\"\nfn produce_i32() -> Int32\n\
        @extern \"C\"\nfn produce_i64() -> Int64\n\
        a = produce_i32()\n  b = produce_i64()\n  a + b\n";
    assert_script_fails_with(source, &["Int, Float, or matching sized numeric"]);
}

#[test]
fn unary_neg_on_sized_int_resolves_to_sized() {
    let source = "@extern \"C\"\nfn produce_i32() -> Int32\n\
        a = produce_i32()\n  -a\n";
    let checked = typecheck(source);
    let expected = global_leaf(&checked, "Int32");
    assert_eq!(trailing_resolution(&checked), expected);
}

#[test]
fn unary_neg_on_unsigned_int_is_rejected() {
    let source = "@extern \"C\"\nfn produce_u8() -> UInt8\n\
        a = produce_u8()\n  -a\n";
    assert_script_fails_with(source, &["signed Int or Float"]);
}
