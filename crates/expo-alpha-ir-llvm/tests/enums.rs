//! IR-text snapshot tests for the enum slice in
//! [`expo_alpha_ir_llvm::emit_llvm_ir`].
//!
//! Three contracts are pinned:
//!
//! - **Pre-emission types**: every [`expo_alpha_ir::IREnumDecl`]
//!   becomes three families of LLVM struct types — the outer
//!   `%Pkg.Enum = type { [N x iAlign] }` blob, the per-variant
//!   complete `%Pkg.Enum.Variant = type { i8, [pad x i8],
//!   %Pkg.Enum.Variant.payload }` (or `{ i8 }` for Unit), and the
//!   per-variant payload `%Pkg.Enum.Variant.payload` struct over
//!   the variant's field types in declaration order.
//! - **`EnumConstruct` lowering**: a `Type.Variant(...)` literal
//!   lowers to alloca-the-outer + `getelementptr` into the
//!   per-variant complete struct + `store` the `i8` tag. Tuple /
//!   struct variants do an extra GEP through the per-variant
//!   payload to write each payload field. The alloca lives in the
//!   function's entry block so reuse across loops doesn't leak
//!   stack.
//! - **Outer blob alignment**: the `iN` chunk type matches the
//!   variant payload's alignment (`i64` on 64-bit hosts when any
//!   payload contains `Int`, falling back to `i8` when every
//!   variant is Unit) and the chunk count is sized to fit the
//!   largest complete variant. Sized assertions are bracketed by a
//!   `#[cfg]` so tests don't break on 32-bit hosts.
//!
//! All assertions are substring-only because LLVM may shuffle
//! attribute ordering between patch versions. Auto-print of an
//! enum return is unsupported in this slice, so the trailing
//! expression in every test is a primitive — the enum lives in a
//! static-method body that gets emitted regardless of whether the
//! script's trailing expression references it.

use expo_alpha_ir_llvm::{emit_llvm_ir, emit_script_llvm_ir};
use expo_ast::util::dedent;

mod common;

use common::{
    APP_NAME, assert_contains, assert_main_shape, lower_program_source as lower_program,
    lower_script_source as lower_script,
};

// ---------------------------------------------------------------------------
// Decl emission — Unit-only enum
// ---------------------------------------------------------------------------

#[test]
fn unit_only_enum_emits_outer_blob_and_per_variant_complete_types() {
    // Both `Red` and `Blue` need to be referenced from emitted code
    // for LLVM to print their type definitions — unreferenced named
    // structs get elided from the textual IR even when their body
    // is set on the context. Two helper functions, one per variant,
    // keep both complete-struct types live in the printed IR.
    let source = "
        enum Color
          Red
          Blue

          fn red -> Color
            Color.Red
          end

          fn blue -> Color
            Color.Blue
          end
        end

        fn main -> Int
          1
        end
        ";

    let program = lower_program(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    // Outer blob: every variant is Unit so max_align == 1, max_size == 1
    // → `{ [1 x i8] }`.
    assert_contains(&ir_text, "%TestApp.Color = type { [1 x i8] }");
    // Per-variant complete: Unit variants carry only the tag.
    assert_contains(&ir_text, "%TestApp.Color.Red = type { i8 }");
    assert_contains(&ir_text, "%TestApp.Color.Blue = type { i8 }");
    // Unit variants have no payload struct.
    assert!(
        !ir_text.contains("%TestApp.Color.Red.payload"),
        "Unit variants should not emit a payload struct.\nIR:\n{ir_text}",
    );
}

// ---------------------------------------------------------------------------
// Decl emission — Tuple variant
// ---------------------------------------------------------------------------

#[test]
fn tuple_variant_emits_payload_struct_and_complete_with_padding() {
    // Constructing both `Ok` and `Err` (instead of just `Ok`) keeps
    // both per-variant complete types live in the printed IR — see
    // the docstring on `unit_only_enum_emits_outer_blob_...` above
    // for why unreferenced named structs are elided.
    let source = "
        enum Result
          Ok(Int)
          Err

          fn ok -> Result
            Result.Ok(42)
          end

          fn err -> Result
            Result.Err
          end
        end

        fn main -> Int
          1
        end
        ";

    let program = lower_program(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "%TestApp.Result.Ok.payload = type { i64 }");
    // Per-variant complete: tag, padding (7 bytes for i64 alignment),
    // payload struct.
    assert_contains(
        &ir_text,
        "%TestApp.Result.Ok = type { i8, [7 x i8], %TestApp.Result.Ok.payload }",
    );
    assert_contains(&ir_text, "%TestApp.Result.Err = type { i8 }");
}

// ---------------------------------------------------------------------------
// Decl emission — Struct variant
// ---------------------------------------------------------------------------

#[test]
fn struct_variant_emits_payload_struct_and_complete_with_padding() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}

          fn unit -> Shape
            Shape.Rect{w: 0, h: 0}
          end
        end

        fn main -> Int
          1
        end
        ";

    let program = lower_program(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "%TestApp.Shape.Rect.payload = type { i64, i64 }");
    assert_contains(
        &ir_text,
        "%TestApp.Shape.Rect = type { i8, [7 x i8], %TestApp.Shape.Rect.payload }",
    );
}

// ---------------------------------------------------------------------------
// EnumConstruct emission
// ---------------------------------------------------------------------------

#[test]
fn unit_variant_construction_lowers_to_alloca_and_tag_store() {
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

    let program = lower_program(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_contains(&ir_text, "alloca %TestApp.Color");
    // Tag-only store at field index 0 of the per-variant complete type.
    assert_contains(&ir_text, "getelementptr inbounds %TestApp.Color.Red");
    assert_contains(&ir_text, "store i8 0");
}

#[test]
fn higher_position_variant_uses_higher_tag_value() {
    let source = "
        enum Color
          Red
          Green
          Blue

          fn last -> Color
            Color.Blue
          end
        end

        fn main -> Int
          1
        end
        ";

    let program = lower_program(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    // `Blue` is the third variant (position 2) so its tag is `i8 2`.
    assert_contains(&ir_text, "store i8 2");
}

#[test]
fn tuple_variant_construction_writes_tag_and_payload_field() {
    let source = "
        enum Result
          Ok(Int)
          Err

          fn ok -> Result
            Result.Ok(42)
          end
        end

        fn main -> Int
          1
        end
        ";

    let program = lower_program(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    // Tag at field 0 of the complete struct.
    assert_contains(&ir_text, "store i8 0");
    // Payload GEP through the per-variant complete struct (field 2,
    // after tag + padding).
    assert_contains(&ir_text, "getelementptr inbounds %TestApp.Result.Ok");
    // Payload field GEP through the payload struct.
    assert_contains(
        &ir_text,
        "getelementptr inbounds %TestApp.Result.Ok.payload",
    );
    assert_contains(&ir_text, "store i64 42");
}

#[test]
fn struct_variant_construction_writes_tag_and_each_named_field() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}

          fn rect -> Shape
            Shape.Rect{w: 10, h: 20}
          end
        end

        fn main -> Int
          1
        end
        ";

    let program = lower_program(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_contains(&ir_text, "alloca %TestApp.Shape");
    assert_contains(&ir_text, "store i8 0");
    assert_contains(
        &ir_text,
        "getelementptr inbounds %TestApp.Shape.Rect.payload",
    );
    assert_contains(&ir_text, "store i64 10");
    assert_contains(&ir_text, "store i64 20");
}

// ---------------------------------------------------------------------------
// Outer blob alignment chunks (64-bit only)
// ---------------------------------------------------------------------------

#[cfg(target_pointer_width = "64")]
#[test]
fn outer_blob_uses_i64_chunks_when_payload_contains_int() {
    let source = "
        enum Result
          Ok(Int)
          Err

          fn ok -> Result
            Result.Ok(42)
          end
        end

        fn main -> Int
          1
        end
        ";

    let program = lower_program(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    // `Ok(Int)`'s complete struct is 16 bytes (tag + 7 bytes padding +
    // i64 payload). The outer blob is `{ [2 x i64] }` — 2 i64 chunks
    // = 16 bytes, alignment 8.
    assert_contains(&ir_text, "%TestApp.Result = type { [2 x i64] }");
}

// ---------------------------------------------------------------------------
// Cross-decl payloads
// ---------------------------------------------------------------------------

#[test]
fn struct_variant_carrying_user_struct_resolves_field_through_pre_emit() {
    let source = "
        struct Inner
          n: Int
        end

        enum Wrap
          Some{value: Inner}

          fn make -> Wrap
            Wrap.Some{value: Inner{n: 7}}
          end
        end

        fn main -> Int
          1
        end
        ";

    let program = lower_program(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_contains(&ir_text, "%TestApp.Inner = type { i64 }");
    assert_contains(
        &ir_text,
        "%TestApp.Wrap.Some.payload = type { %TestApp.Inner }",
    );
}

// ---------------------------------------------------------------------------
// Static method dispatch
// ---------------------------------------------------------------------------

#[test]
fn static_method_returning_enum_emits_outer_typed_signature() {
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

    let program = lower_program(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_contains(&ir_text, "define %TestApp.Color @TestApp.Color.primary()");
}

// ---------------------------------------------------------------------------
// Script mode: enum construction from a non-trailing position
// ---------------------------------------------------------------------------

#[test]
fn script_can_emit_enum_construct_from_non_trailing_assignment() {
    // Trailing expression is `1`, so the script's return type is
    // `Int` and auto-print uses `__expo_alpha_print_i64`. The
    // `c = Color.Red` assignment exercises EnumConstruct + a local
    // slot typed `IRType::Enum(_)`.
    let source = "
        enum Color
          Red
          Blue
        end

        c = Color.Red
        1
        ";

    let script = lower_script(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "alloca %TestApp.Color");
    assert_contains(&ir_text, "store i8 0");
    assert_contains(&ir_text, "call void @__expo_alpha_print_i64");
}
