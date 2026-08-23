use koja_ast::util::dedent;
use koja_ir_llvm::emit_script_llvm_ir;

mod common;

use common::{APP_NAME, assert_contains, lower_script_source};

#[test]
fn emits_distinct_overload_and_generic_symbols() {
    let script = lower_script_source(&dedent(
        "
        fn choose(value: Int) -> Int
          value
        end

        fn choose(left: Int, right: Int) -> Int
          left + right
        end

        fn identity<T>(value: T) -> T
          value
        end

        fn identity<T>(first: T, second: T) -> T
          second
        end

        choose(1)
        choose(2, 3)
        identity(4)
        identity(5, 6)
        ",
    ));
    let llvm = emit_script_llvm_ir(&script, APP_NAME).expect("LLVM emission should succeed");

    assert_contains(&llvm, "TestApp.choose/1");
    assert_contains(&llvm, "TestApp.choose/2");
    assert_contains(&llvm, "TestApp.identity/1_$Int64$");
    assert_contains(&llvm, "TestApp.identity/2_$Int64$");
}
