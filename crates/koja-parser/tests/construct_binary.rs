//! Binary-literal construction: empty, plain, with sizes, with
//! type annotations, and with the byte/unit/signedness/endianness
//! modifier suffix. Also covers the `>>` ambiguity (generics like
//! `List<Option<Int>>` must still parse cleanly when the lexer
//! produces `GtGt`) and the bodyless-`extern`-function path.

use koja_ast::ast::{
    BinaryEndianness, BinarySignedness, BinaryUnit, Expr, ExprKind, Item, Statement,
};
use koja_parser::{ParseMode, parse};

#[test]
fn test_binary_literal_empty() {
    let result = parse("fn main\n  x = <<>>\nend\n", ParseMode::File);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let func = match &result.ast.items[0] {
        Item::Function(f) => f,
        _ => panic!("expected function"),
    };
    match &func.body.as_ref().unwrap()[0] {
        Statement::Assignment {
            value:
                Expr {
                    kind: ExprKind::BinaryLiteral { segments, .. },
                    ..
                },
            ..
        } => assert!(segments.is_empty()),
        other => panic!("expected binary literal, got {other:?}"),
    }
}

#[test]
fn test_binary_literal_segments() {
    let result = parse("fn main\n  x = <<0xFF, 0x00>>\nend\n", ParseMode::File);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let func = match &result.ast.items[0] {
        Item::Function(f) => f,
        _ => panic!("expected function"),
    };
    match &func.body.as_ref().unwrap()[0] {
        Statement::Assignment {
            value:
                Expr {
                    kind: ExprKind::BinaryLiteral { segments, .. },
                    ..
                },
            ..
        } => assert_eq!(segments.len(), 2),
        other => panic!("expected binary literal, got {other:?}"),
    }
}

#[test]
fn test_binary_literal_with_size() {
    let result = parse(
        "fn main\n  x = <<header::8, length::16 big>>\nend\n",
        ParseMode::File,
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let func = match &result.ast.items[0] {
        Item::Function(f) => f,
        _ => panic!("expected function"),
    };
    match &func.body.as_ref().unwrap()[0] {
        Statement::Assignment {
            value:
                Expr {
                    kind: ExprKind::BinaryLiteral { segments, .. },
                    ..
                },
            ..
        } => {
            assert_eq!(segments.len(), 2);
            assert!(segments[0].size.is_some());
            assert!(segments[1].size.is_some());
            assert_eq!(segments[1].endianness, Some(BinaryEndianness::Big));
        }
        other => panic!("expected binary literal, got {other:?}"),
    }
}

#[test]
fn test_binary_literal_with_type() {
    let result = parse("fn main\n  x = <<value: Int>>\nend\n", ParseMode::File);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let func = match &result.ast.items[0] {
        Item::Function(f) => f,
        _ => panic!("expected function"),
    };
    match &func.body.as_ref().unwrap()[0] {
        Statement::Assignment {
            value:
                Expr {
                    kind: ExprKind::BinaryLiteral { segments, .. },
                    ..
                },
            ..
        } => {
            assert_eq!(segments.len(), 1);
            assert!(segments[0].type_ann.is_some());
            assert!(segments[0].size.is_none());
        }
        other => panic!("expected binary literal, got {other:?}"),
    }
}

#[test]
fn test_binary_literal_byte_modifier() {
    let result = parse(
        "fn main\n  x = <<data::32 byte unsigned little>>\nend\n",
        ParseMode::File,
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let func = match &result.ast.items[0] {
        Item::Function(f) => f,
        _ => panic!("expected function"),
    };
    match &func.body.as_ref().unwrap()[0] {
        Statement::Assignment {
            value:
                Expr {
                    kind: ExprKind::BinaryLiteral { segments, .. },
                    ..
                },
            ..
        } => {
            assert_eq!(segments.len(), 1);
            assert_eq!(segments[0].unit, BinaryUnit::Byte);
            assert_eq!(segments[0].signedness, Some(BinarySignedness::Unsigned));
            assert_eq!(segments[0].endianness, Some(BinaryEndianness::Little));
        }
        other => panic!("expected binary literal, got {other:?}"),
    }
}

#[test]
fn test_binary_pattern() {
    let result = parse(
        "fn main\n  match msg\n    <<tag::8, payload::16 big>> -> tag\n    <<>> -> 0\n  end\nend\n",
        ParseMode::File,
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
}

#[test]
fn test_generics_still_work_after_gtgt() {
    let result = parse(
        "fn main\n  x: List<Option<Int>> = []\nend\n",
        ParseMode::File,
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
}

#[test]
fn test_extern_c_bodyless_function() {
    let result = parse(
        "@extern \"C\"\nfn argon2id_hash(t: UInt32, m: UInt32) -> Int32\n",
        ParseMode::File,
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let func = match &result.ast.items[0] {
        Item::Function(f) => f,
        _ => panic!("expected function"),
    };
    assert_eq!(func.name, "argon2id_hash");
    assert!(func.body.is_none(), "extern C function should have no body");
    assert_eq!(func.annotations.len(), 1);
    assert_eq!(func.annotations[0].name, "extern");
}

#[test]
fn test_extern_c_struct_per_function_annotations() {
    let result = parse(
        "struct Argon2C\n  @extern \"C\" @link \"argon2\"\n  fn hash(t: UInt32) -> Int32\n  @extern \"C\" @link \"argon2\"\n  fn verify(e: UInt32) -> Int32\nend\n",
        ParseMode::File,
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let s = match &result.ast.items[0] {
        Item::Struct(s) => s,
        _ => panic!("expected struct"),
    };
    assert_eq!(s.name, "Argon2C");
    assert_eq!(s.functions.len(), 2);
    assert!(s.functions[0].body.is_none());
    assert!(s.functions[1].body.is_none());
    assert_eq!(s.functions[0].annotations.len(), 2);
    assert_eq!(s.functions[1].annotations.len(), 2);
}

#[test]
fn test_regular_function_still_has_body() {
    let result = parse(
        "fn add(a: Int32, b: Int32) -> Int32\n  a + b\nend\n",
        ParseMode::File,
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let func = match &result.ast.items[0] {
        Item::Function(f) => f,
        _ => panic!("expected function"),
    };
    assert!(func.body.is_some(), "regular function should have body");
    assert!(!func.body.as_ref().unwrap().is_empty());
}
