use koja_ir_llvm::emit_script_llvm_ir;

mod common;

use common::{APP_NAME, assert_contains, lower_script_source};

#[test]
fn negative_cptr_counts_emit_panic_guards() {
    let script = lower_script_source(
        "
        ptr: CPtr<UInt8> = CPtr.alloc(1)
        ptr.to_binary(1)
        ptr.free()
        ",
    );
    let llvm = emit_script_llvm_ir(&script, APP_NAME).expect("emit LLVM");
    assert_contains(&llvm, "icmp slt i64");
    assert_contains(&llvm, "negative_panic");
    assert_contains(&llvm, "@__koja_panic");
    assert_contains(&llvm, "CPtr.alloc count cannot be negative");
    assert_contains(&llvm, "CPtr.to_binary length cannot be negative");
}
