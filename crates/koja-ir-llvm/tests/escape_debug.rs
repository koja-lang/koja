//! IR-text + behavioral coverage for `Debug for String`,
//! specifically `escape_debug`, which v1-PARITY pinned as the
//! "match-on-String + concat-assign" SIGABRT source. The bug was a
//! spurious `DropLocal` inside the second match arm at lowering
//! time. Under the value-semantics leak baseline the arm bodies
//! carry no `free` calls and the function emits no drops at all.
//!
//! These tests pin the LLVM IR shape. The actual end-to-end
//! "compile + execute the binary, observe correct output" coverage
//! lives in the driver crate (see `alpha_two_plus_two.rs`).

use koja_ast::util::dedent;
use koja_ir_llvm::emit_script_llvm_ir;

mod common;

use common::{APP_NAME, extract_function_body, lower_script_source as lower};

#[test]
fn escape_debug_emits_no_free_call_inside_match_arm_bodies() {
    // The escape_debug body is exactly:
    //   result = ""
    //   for c in self
    //     match c
    //       "\\" -> result = result <> "\\\\"
    //       ... 5 more arms ...
    //     end
    //   end
    //   result
    //
    // Pre-fix, the second-and-later arms each carried a `call void
    // @free(...)` against the literal-Unowned slot in their body
    // block. After the fix every arm starts from the construct-entry
    // snapshot (Unowned, since `result = ""` made it Unowned) and no
    // arm emits a `free` against the slot.
    //
    // We drive the `String.escape_debug` function from the auto-
    // imported `Global.string` source, since lowering it is what
    // walks through the buggy code path. The test asserts the
    // emitted body carries no `call void @koja_free` at all (the
    // drop insertion is deferred under the value-semantics leak
    // baseline, so no free lands).
    let script = lower(&dedent(
        "
        \"hi\".escape_debug()
        ",
    ));
    let ir_text = emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir");
    let body = extract_function_body(&ir_text, "Global.String.escape_debug");
    assert!(
        !body.contains("call void @koja_free"),
        "expected no `call void @koja_free` inside `Global.String.escape_debug` body \
         (the cross-arm slot-state leak was synthesizing free calls inside \
         match arm bodies); got body:\n{body}",
    );
}

#[test]
fn debug_format_for_string_compiles_without_free_in_concat_chain() {
    // `Debug for String::format` is the user-facing surface and
    // looks like `"\"" <> self.escape_debug() <> "\""`. Pre-fix this
    // didn't even surface as an emit-time error. The SIGABRT only
    // fired at runtime when `escape_debug`'s `match` arm body
    // executed `free` on a literal. With the fix in place we can
    // emit the IR and observe that the surrounding format body's
    // single function-exit drop is the only `free` site.
    let script = lower(&dedent(
        "
        \"hi\".format()
        ",
    ));
    let ir_text = emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir");
    let body = extract_function_body(&ir_text, "Global.String.format");
    // `format` always returns a freshly-concat'd heap string. Under
    // the leak baseline no drops are emitted in the body.
    assert!(
        !body.contains("call void @koja_free"),
        "expected no `call void @koja_free` inside `Global.String.format` body; got:\n{body}",
    );
}
