//! IR-text snapshot tests for monomorphized generic functions.
//!
//! Pins that every concrete instantiation produced by
//! [`expo_ir::generics::instantiate`] reaches LLVM as a
//! distinct `define` carrying the mangled `_$arg.arg$` symbol:
//!
//! - `id<Int>(x)` → `define i64 @"TestApp.id_$Int64$"(i64 %x)`
//! - `id<String>(x)` → `define ptr @"TestApp.id_$String$"(ptr %x)`
//! - method on a generic struct → `@"TestApp.Pair_$Int64.String$.first"`
//! - distinct args at the same call site → distinct `define`s
//!
//! The LLVM backend has zero generics-aware code; it lowers whatever
//! decls land in the [`expo_ir::IRPackage::functions`] map. A
//! green test here pins that the closure-pass result threads through
//! emit unchanged.
//!
//! Substring-only assertions (LLVM may shuffle attribute ordering
//! between patch versions).

use expo_ast::util::dedent;
use expo_ir_llvm::{emit_llvm_ir, emit_script_llvm_ir};

mod common;

use common::{
    APP_NAME, assert_contains, assert_main_shape, lower_program_source as lower_program,
    lower_script_source as lower_script,
};

#[test]
fn identity_function_distinct_args_emit_distinct_defines() {
    let source = "
        fn id<T>(x: T) -> T
          x
        end

        id(42)
        id(\"hello\")
        1
        ";

    let script = lower_script(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "define i64 @\"TestApp.id_$Int64$\"(i64");
    assert_contains(&ir_text, "define ptr @\"TestApp.id_$String$\"(ptr");
    assert!(
        !ir_text.contains("@TestApp.id("),
        "generic template `@TestApp.id` must not appear as a defined LLVM function:\n{ir_text}",
    );
}

#[test]
fn generic_function_call_site_targets_mangled_symbol() {
    let source = "
        fn id<T>(x: T) -> T
          x
        end

        id(42)
        ";

    let script = lower_script(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(&ir_text, "call i64 @\"TestApp.id_$Int64$\"(i64 42)");
}

#[test]
fn method_on_generic_struct_emits_define_with_struct_mangled_prefix() {
    let source = "
        struct Pair<T, U>
          a: T
          b: U

          fn first(self) -> T
            self.a
          end
        end

        fn main -> Int
          p = Pair{a: 1, b: \"x\"}
          p.first()
        end
        ";

    let program = lower_program(&dedent(source));
    let ir_text = emit_llvm_ir(&program, APP_NAME).expect("emit_llvm_ir should succeed");

    assert_main_shape(&ir_text);
    assert_contains(
        &ir_text,
        "define i64 @\"TestApp.Pair_$Int64.String$.first\"",
    );
    assert_contains(&ir_text, "call i64 @\"TestApp.Pair_$Int64.String$.first\"");
    assert!(
        !ir_text.contains("@TestApp.Pair.first("),
        "generic template `@TestApp.Pair.first` must not appear as a defined LLVM function:\n{ir_text}",
    );
}
