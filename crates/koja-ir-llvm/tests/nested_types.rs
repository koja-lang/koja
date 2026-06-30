//! IR-text coverage for nested type names: a nested struct emits a
//! distinctly-mangled LLVM type and its `extend` method emits a
//! distinctly-mangled symbol, so nesting never collides with a
//! top-level type of the same leaf name.

use koja_ast::util::dedent;
use koja_ir_llvm::emit_script_llvm_ir;

mod common;

use common::{APP_NAME, assert_contains, lower_script_source as lower};

#[test]
fn nested_struct_and_method_mangle_with_full_path() {
    let source = "
        struct Outer
          tag: Int
        end

        struct Outer.Inner
          x: Int
        end

        extend Outer.Inner
          fn doubled(self) -> Int
            self.x * 2
          end
        end

        Outer.Inner{x: 21}.doubled()
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");

    assert_contains(&ir_text, "%TestApp.Outer.Inner = type { i64 }");
    assert_contains(&ir_text, "@TestApp.Outer.Inner.doubled(");
}
