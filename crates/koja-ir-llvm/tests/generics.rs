//! IR-text snapshot tests for monomorphized generic decls.
//!
//! Pins that every concrete instantiation surfaced by
//! [`koja_ir::generics::instantiate`] reaches LLVM as a
//! distinct named type carrying the mangled `_$arg.arg$` symbol:
//!
//! - `Pair<Int, String>` -> `%TestApp.Pair_$Int64.String$ = type { i64, ptr }`
//! - `Box<T>.Of(...)` -> outer / complete / payload types keyed at
//!   `%TestApp.Box_$Int64$` (with the `.Of` / `.Of.payload` suffixes
//!   each variant carries on the non-generic path).
//! - Distinct `(template, args)` pairs -> distinct LLVM types.
//! - Nested generics (`Pair<Box<Int>, String>`) -> both inner and
//!   outer surface as named types, with the outer's first slot
//!   referencing the inner by name.
//!
//! The LLVM backend has zero generics-aware code: it lowers
//! whatever decls land in the [`koja_ir::IRPackage`] map. A
//! green test here confirms the closure-pass result stays
//! observable end-to-end.
//!
//! Substring-only assertions (LLVM may shuffle attribute / SSA
//! numbering between patch versions). Auto-print of an enum return
//! is unsupported in this slice, so trailing expressions are
//! primitives and the construction lives in a preceding statement
//! whose `alloca` / `getelementptr` keep the named types live.

use koja_ast::util::dedent;
use koja_ir_llvm::emit_script_llvm_ir;

mod common;

use common::{APP_NAME, assert_contains, assert_main_shape, lower_script_source as lower};

// ---------------------------------------------------------------------------
// Generic structs
// ---------------------------------------------------------------------------

#[test]
fn generic_struct_emits_named_llvm_struct_with_mangled_symbol() {
    let source = "
        struct Pair<T, U>
          a: T
          b: U
        end

        Pair{a: 1, b: \"x\"}
        1
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(
        &ir_text,
        "%\"TestApp.Pair_$Int64.String$\" = type { i64, ptr }",
    );
    assert_contains(&ir_text, "alloca %\"TestApp.Pair_$Int64.String$\"");
    assert_contains(
        &ir_text,
        "getelementptr inbounds %\"TestApp.Pair_$Int64.String$\"",
    );
    assert_contains(&ir_text, "store i64 1");
    assert!(
        !ir_text.contains("%TestApp.Pair "),
        "generic template `%TestApp.Pair` must not appear as a named LLVM type:\n{ir_text}",
    );
}

#[test]
fn generic_struct_distinct_args_emit_distinct_named_llvm_types() {
    let source = "
        struct Pair<T, U>
          a: T
          b: U
        end

        Pair{a: 1, b: \"x\"}
        Pair{a: \"y\", b: 2}
        1
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(
        &ir_text,
        "%\"TestApp.Pair_$Int64.String$\" = type { i64, ptr }",
    );
    assert_contains(
        &ir_text,
        "%\"TestApp.Pair_$String.Int64$\" = type { ptr, i64 }",
    );
}

#[test]
fn nested_generic_struct_emits_inner_type_inside_outer_field_layout() {
    let source = "
        struct Box<T>
          value: T
        end

        struct Pair<A, B>
          a: A
          b: B
        end

        Pair{a: Box{value: 42}, b: \"x\"}
        1
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "%\"TestApp.Box_$Int64$\" = type { i64 }");
    // Outer's first slot references the inner concrete decl by name.
    assert_contains(
        &ir_text,
        "%\"TestApp.Pair_$TestApp.Box_$Int64$.String$\" = type { %\"TestApp.Box_$Int64$\", ptr }",
    );
}

// ---------------------------------------------------------------------------
// Generic enums
// ---------------------------------------------------------------------------

#[test]
fn generic_enum_emits_outer_complete_and_payload_named_types_with_mangled_symbol() {
    let source = "
        enum Box<T>
          Of(T)
        end

        Box.Of(42)
        1
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    // Outer blob — sized to the largest variant's complete struct.
    assert_contains(&ir_text, "%\"TestApp.Box_$Int64$\" = type {");
    // Per-variant complete struct with i8 tag + payload.
    assert_contains(&ir_text, "%\"TestApp.Box_$Int64$.Of\" = type {");
    // Per-variant payload struct over the substituted Int payload.
    assert_contains(
        &ir_text,
        "%\"TestApp.Box_$Int64$.Of.payload\" = type { i64 }",
    );
    assert!(
        !ir_text.contains("%TestApp.Box "),
        "generic template `%TestApp.Box` must not appear as a named LLVM type:\n{ir_text}",
    );
}

#[test]
fn generic_enum_distinct_args_emit_distinct_named_llvm_types() {
    let source = "
        enum Box<T>
          Of(T)
        end

        Box.Of(42)
        Box.Of(\"x\")
        1
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(
        &ir_text,
        "%\"TestApp.Box_$Int64$.Of.payload\" = type { i64 }",
    );
    assert_contains(
        &ir_text,
        "%\"TestApp.Box_$String$.Of.payload\" = type { ptr }",
    );
}
