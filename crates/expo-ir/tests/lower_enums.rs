//! IR-lowering coverage for the enum slice: `lower/enums.rs`.
//!
//! Walks every variant shape (Unit, Tuple, Struct) end-to-end and
//! pins:
//!
//! - `IRPackage::enums` carries the lifted [`IREnumDecl`] keyed at
//!   the mangled [`IRSymbol`], with dense 0..n tags, declaration-
//!   ordered variants, and translated payload [`IRType`]s.
//! - `Color.Red`, `Result.Ok(42)`, `Shape.Rect{w: 1, h: 2}` lower
//!   to [`IRInstruction::EnumConstruct`] with the matching
//!   [`EnumPayloadInit`] shape: `Unit` carries no operands, `Tuple`
//!   carries one [`ValueId`] per positional element, `Struct`
//!   carries one [`StructFieldInit`] per declared field
//!   canonicalized to declaration order regardless of AST input
//!   order.
//! - `IRType::Enum(symbol)` flows through the SSA value's static
//!   type slot.
//! - Inline + impl-block static methods on enum receivers lower to
//!   regular package functions keyed at `<package>.<enum>.<method>`.

use expo_ast::util::dedent;
use expo_ir::{
    EnumPayloadInit, FunctionKind, IREnumDecl, IRInstruction, IRProgram, IRType, IRVariantPayload,
    IRVariantTag,
};

mod common;

use common::{PACKAGE, lower_program_source, lower_script_source};

fn enum_decl<'a>(program: &'a IRProgram, name: &str) -> &'a IREnumDecl {
    let mangled = format!("{PACKAGE}.{name}");
    program
        .enum_decl(&mangled)
        .unwrap_or_else(|| panic!("enum `{mangled}` missing from IRProgram"))
}

fn first_enum_construct(
    block: &expo_ir::IRBasicBlock,
) -> (
    expo_ir::ValueId,
    &EnumPayloadInit,
    IRVariantTag,
    &expo_ir::IRSymbol,
) {
    block
        .instructions
        .iter()
        .find_map(|inst| match inst {
            IRInstruction::EnumConstruct {
                dest,
                payload,
                tag,
                ty,
            } => Some((*dest, payload, *tag, ty)),
            _ => None,
        })
        .expect("expected one EnumConstruct in block")
}

// ---------------------------------------------------------------------------
// Decl lowering
// ---------------------------------------------------------------------------

#[test]
fn unit_only_enum_lowers_with_dense_declaration_order_tags() {
    let source = "
        enum Color
          Red
          Green
          Blue
        end

        fn main -> Int
          1
        end
        ";

    let program = lower_program_source(&dedent(source));
    let decl = enum_decl(&program, "Color");
    assert_eq!(decl.symbol.mangled(), "TestApp.Color");
    let names: Vec<&str> = decl.variants.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, vec!["Red", "Green", "Blue"]);
    let tags: Vec<u8> = decl.variants.iter().map(|v| v.tag.0).collect();
    assert_eq!(tags, vec![0, 1, 2]);
    for variant in &decl.variants {
        assert!(
            matches!(variant.payload, IRVariantPayload::Unit),
            "expected Unit shape for `{}`, got {:?}",
            variant.name,
            variant.payload,
        );
    }
}

#[test]
fn tuple_variant_lowers_with_translated_element_types() {
    let source = "
        enum Result
          Ok(Int)
          Err(String)
        end

        fn main -> Int
          1
        end
        ";

    let program = lower_program_source(&dedent(source));
    let decl = enum_decl(&program, "Result");
    assert_eq!(decl.variants.len(), 2);

    let ok = &decl.variants[0];
    assert_eq!(ok.name, "Ok");
    match &ok.payload {
        IRVariantPayload::Tuple(types) => {
            assert_eq!(types, &vec![IRType::Int64]);
        }
        other => panic!("expected Tuple([Int64]), got {other:?}"),
    }

    let err = &decl.variants[1];
    assert_eq!(err.name, "Err");
    match &err.payload {
        IRVariantPayload::Tuple(types) => {
            assert_eq!(types, &vec![IRType::String]);
        }
        other => panic!("expected Tuple([String]), got {other:?}"),
    }
}

#[test]
fn struct_variant_lowers_with_dense_declaration_order_field_indices() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}
        end

        fn main -> Int
          1
        end
        ";

    let program = lower_program_source(&dedent(source));
    let decl = enum_decl(&program, "Shape");
    assert_eq!(decl.variants.len(), 1);
    let rect = &decl.variants[0];
    match &rect.payload {
        IRVariantPayload::Struct(fields) => {
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].name, "w");
            assert_eq!(fields[0].index, 0);
            assert_eq!(fields[0].ir_type, IRType::Int64);
            assert_eq!(fields[1].name, "h");
            assert_eq!(fields[1].index, 1);
            assert_eq!(fields[1].ir_type, IRType::Int64);
        }
        other => panic!("expected Struct(_), got {other:?}"),
    }
}

#[test]
fn mixed_shape_enum_preserves_per_variant_payload_kind() {
    let source = "
        enum Shape
          Empty
          Circle(Int)
          Rect{w: Int, h: Int}
        end

        fn main -> Int
          1
        end
        ";

    let program = lower_program_source(&dedent(source));
    let decl = enum_decl(&program, "Shape");
    let kinds: Vec<&str> = decl
        .variants
        .iter()
        .map(|v| match v.payload {
            IRVariantPayload::Unit => "Unit",
            IRVariantPayload::Tuple(_) => "Tuple",
            IRVariantPayload::Struct(_) => "Struct",
        })
        .collect();
    assert_eq!(kinds, vec!["Unit", "Tuple", "Struct"]);
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

#[test]
fn unit_variant_construction_lowers_to_unit_payload_init() {
    let source = "
        enum Color
          Red
          Blue
        end

        Color.Red
        ";

    let script = lower_script_source(&dedent(source));
    let block = script.blocks.first().expect("script has one block");
    let (_dest, payload, tag, ty) = first_enum_construct(block);
    assert_eq!(ty.mangled(), "TestApp.Color");
    assert_eq!(tag, IRVariantTag(0));
    assert!(matches!(payload, EnumPayloadInit::Unit));
    assert_eq!(script.return_type, IRType::Enum(ty.clone()));
}

#[test]
fn tuple_variant_construction_lowers_to_tuple_payload_init() {
    let source = "
        enum Result
          Ok(Int)
          Err(String)
        end

        Result.Ok(42)
        ";

    let script = lower_script_source(&dedent(source));
    let block = script.blocks.first().expect("script has one block");
    let (_dest, payload, tag, ty) = first_enum_construct(block);
    assert_eq!(ty.mangled(), "TestApp.Result");
    assert_eq!(tag, IRVariantTag(0));
    let EnumPayloadInit::Tuple(values) = payload else {
        panic!("expected Tuple payload, got {payload:?}");
    };
    assert_eq!(values.len(), 1);
}

#[test]
fn struct_variant_construction_canonicalizes_field_init_order() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}
        end

        Shape.Rect{h: 2, w: 1}
        ";

    let script = lower_script_source(&dedent(source));
    let block = script.blocks.first().expect("script has one block");
    let (_dest, payload, tag, ty) = first_enum_construct(block);
    assert_eq!(ty.mangled(), "TestApp.Shape");
    assert_eq!(tag, IRVariantTag(0));
    let EnumPayloadInit::Struct(inits) = payload else {
        panic!("expected Struct payload, got {payload:?}");
    };
    assert_eq!(inits.len(), 2);
    assert_eq!(inits[0].index, 0);
    assert_eq!(inits[1].index, 1);
}

#[test]
fn variant_at_higher_position_carries_correct_tag() {
    let source = "
        enum Color
          Red
          Green
          Blue
        end

        Color.Blue
        ";

    let script = lower_script_source(&dedent(source));
    let block = script.blocks.first().expect("script has one block");
    let (_dest, _payload, tag, _ty) = first_enum_construct(block);
    assert_eq!(tag, IRVariantTag(2));
}

// ---------------------------------------------------------------------------
// Static methods
// ---------------------------------------------------------------------------

#[test]
fn inline_static_method_on_enum_lowers_into_package_function_map() {
    let source = "
        enum Color
          Red
          Blue

          fn primary -> Color
            Color.Red
          end
        end

        fn main -> Int
          1
        end
        ";

    let program = lower_program_source(&dedent(source));
    let function = program
        .function("TestApp.Color.primary")
        .expect("inline static method on enum missing from program");
    assert_eq!(function.kind, FunctionKind::Regular);
    assert!(!function.blocks.is_empty(), "method should have a body");
}

#[test]
fn impl_block_on_enum_lowers_static_method_into_package_function_map() {
    let source = "
        enum Color
          Red
          Blue
        end

        extend Color
          fn primary -> Color
            Color.Red
          end
        end

        fn main -> Int
          1
        end
        ";

    let program = lower_program_source(&dedent(source));
    let function = program
        .function("TestApp.Color.primary")
        .expect("impl-block static method on enum missing from program");
    assert_eq!(function.kind, FunctionKind::Regular);
    assert!(!function.blocks.is_empty(), "method should have a body");
}

#[test]
fn static_method_call_on_enum_emits_call_against_qualified_symbol() {
    let source = "
        enum Color
          Red
          Blue

          fn primary -> Color
            Color.Red
          end
        end

        Color.primary()
        ";

    let script = lower_script_source(&dedent(source));
    let block = script.blocks.first().expect("script has one entry block");
    let callee = block
        .instructions
        .iter()
        .find_map(|inst| match inst {
            IRInstruction::Call { callee, .. } => Some(callee.clone()),
            _ => None,
        })
        .expect("expected one Call instruction");
    assert_eq!(callee.mangled(), "TestApp.Color.primary");
}

// ---------------------------------------------------------------------------
// Cross-decl payloads
// ---------------------------------------------------------------------------

#[test]
fn tuple_variant_carrying_user_struct_lowers_to_struct_ir_type() {
    let source = "
        struct Inner
          n: Int
        end

        enum Wrap
          Some(Inner)
          None
        end

        fn main -> Int
          1
        end
        ";

    let program = lower_program_source(&dedent(source));
    let wrap = enum_decl(&program, "Wrap");
    let some = &wrap.variants[0];
    let inner_symbol = program
        .struct_decl("TestApp.Inner")
        .expect("Inner struct missing")
        .symbol
        .clone();
    match &some.payload {
        IRVariantPayload::Tuple(types) => {
            assert_eq!(types, &vec![IRType::Struct(inner_symbol)]);
        }
        other => panic!("expected Tuple([Struct(Inner)]), got {other:?}"),
    }
}

#[test]
fn struct_variant_carrying_user_enum_lowers_to_enum_ir_type() {
    let source = "
        enum Color
          Red
          Blue
        end

        enum Wrap
          Tagged{value: Color}
        end

        fn main -> Int
          1
        end
        ";

    let program = lower_program_source(&dedent(source));
    let wrap = enum_decl(&program, "Wrap");
    let color_symbol = enum_decl(&program, "Color").symbol.clone();
    let tagged = &wrap.variants[0];
    match &tagged.payload {
        IRVariantPayload::Struct(fields) => {
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].name, "value");
            assert_eq!(fields[0].ir_type, IRType::Enum(color_symbol));
        }
        other => panic!("expected Struct payload, got {other:?}"),
    }
}
