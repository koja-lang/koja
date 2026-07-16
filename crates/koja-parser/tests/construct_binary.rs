//! Binary-literal construction: empty, plain, with sizes, with
//! type annotations, and with the byte/unit/signedness/endianness
//! modifier suffix. Also covers the `>>` ambiguity (generics like
//! `List<Option<Int>>` must still parse cleanly when the lexer
//! produces `GtGt`) and the bodyless-`extern`-function path.

use koja_ast::ast::{BinaryEndianness, BinarySegment, BinarySignedness, BinaryUnit, ExprKind};

mod common;

use common::{first_function, first_script_expr, first_struct, parse_clean_script};

fn binary_literal_segments(source: &str) -> Vec<BinarySegment> {
    let expr = first_script_expr(source);
    match expr.kind {
        ExprKind::BinaryLiteral { segments } => segments,
        other => panic!("expected binary literal, got {other:?}"),
    }
}

#[test]
fn test_binary_literal_empty() {
    let segments = binary_literal_segments("x = <<>>");
    assert!(segments.is_empty());
}

#[test]
fn test_binary_literal_segments() {
    let segments = binary_literal_segments("x = <<0xFF, 0x00>>");
    assert_eq!(segments.len(), 2);
}

#[test]
fn test_binary_literal_with_size() {
    let segments = binary_literal_segments("x = <<header::8, length::16 big>>");
    assert_eq!(segments.len(), 2);
    assert!(segments[0].size.is_some());
    assert!(segments[1].size.is_some());
    assert_eq!(segments[1].endianness, Some(BinaryEndianness::Big));
}

#[test]
fn test_binary_literal_with_type() {
    let segments = binary_literal_segments("x = <<value: Int>>");
    assert_eq!(segments.len(), 1);
    assert!(segments[0].type_ann.is_some());
    assert!(segments[0].size.is_none());
}

#[test]
fn test_binary_literal_byte_modifier() {
    let segments = binary_literal_segments("x = <<data::32 byte unsigned little>>");
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].unit, BinaryUnit::Byte);
    assert_eq!(segments[0].signedness, Some(BinarySignedness::Unsigned));
    assert_eq!(segments[0].endianness, Some(BinaryEndianness::Little));
}

#[test]
fn test_binary_pattern() {
    parse_clean_script(
        "
        match msg
          <<tag::8, payload::16 big>> -> tag
          <<>> -> 0
        end
        ",
    );
}

#[test]
fn test_generics_still_work_after_gtgt() {
    first_script_expr("x: List<Option<Int>> = []");
}

#[test]
fn test_extern_c_bodyless_function() {
    let func = first_function(
        r#"
        @extern "C"
        fn argon2id_hash(t: UInt32, m: UInt32) -> Int32
        "#,
    );
    assert_eq!(func.name, "argon2id_hash");
    assert!(func.body.is_none(), "extern C function should have no body");
    assert_eq!(func.annotations.len(), 1);
    assert_eq!(func.annotations[0].name, "extern");
}

#[test]
fn test_extern_c_struct_per_function_annotations() {
    let s = first_struct(
        r#"
        struct Argon2C
          @extern "C" @link "argon2"
          fn hash(t: UInt32) -> Int32
          @extern "C" @link "argon2"
          fn verify(e: UInt32) -> Int32
        end
        "#,
    );
    assert_eq!(s.name(), "Argon2C");
    assert_eq!(s.functions.len(), 2);
    assert!(s.functions[0].body.is_none());
    assert!(s.functions[1].body.is_none());
    assert_eq!(s.functions[0].annotations.len(), 2);
    assert_eq!(s.functions[1].annotations.len(), 2);
}

#[test]
fn test_regular_function_still_has_body() {
    let func = first_function(
        "
        fn add(a: Int32, b: Int32) -> Int32
          a + b
        end
        ",
    );
    assert!(func.body.is_some(), "regular function should have body");
    assert!(!func.body.as_ref().unwrap().is_empty());
}
