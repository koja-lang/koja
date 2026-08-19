//! IR-text pins for the consuming-twin emitters
//! ([`koja_ir::IRIntrinsicId::Consuming`], `.$consume$` symbols).
//! A rebind loop's call site must route through the twin, the list
//! twin's body must carry the in-place / grow split, and the
//! hashtable twins must skip the copy-on-write buffer clone their
//! copying originals perform.

use koja_ast::util::dedent;
use koja_ir_llvm::emit_script_llvm_ir;

mod common;

use common::assert_contains;

const APP_NAME: &str = "consuming_test";

fn emit(source: &str) -> String {
    let script = common::lower_script_source(&dedent(source));
    emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed")
}

/// The body of the unique `define` whose header line contains every
/// needle, sliced to its closing brace.
fn body_of_define<'a>(ir_text: &'a str, needles: &[&str]) -> &'a str {
    let header_start = ir_text
        .lines()
        .find(|line| line.starts_with("define") && needles.iter().all(|n| line.contains(n)))
        .map(|line| ir_text.find(line).unwrap())
        .unwrap_or_else(|| panic!("no `define` containing {needles:?} in:\n{ir_text}"));
    let body = &ir_text[header_start..];
    let body_end = body
        .find("\n}")
        .unwrap_or_else(|| panic!("define containing {needles:?} has no closing brace"));
    &body[..body_end]
}

#[test]
fn rebind_loop_call_site_routes_through_the_append_twin() {
    let source = "
        xs: List<Int> = []
        i = 0
        while i < 3
          xs = xs.append(i)
          i += 1
        end
        xs.length().print()
        ";

    let ir_text = emit(source);
    let call_line = ir_text
        .lines()
        .find(|line| line.contains("call") && line.contains(".$consume$"))
        .unwrap_or_else(|| panic!("no call to a `.$consume$` twin in:\n{ir_text}"));
    assert!(
        call_line.contains("append"),
        "expected the fused call to target the append twin, got: {call_line}",
    );
}

#[test]
fn append_twin_body_splits_in_place_and_grow() {
    let source = "
        xs: List<Int> = []
        i = 0
        while i < 3
          xs = xs.append(i)
          i += 1
        end
        xs.length().print()
        ";

    let ir_text = emit(source);
    let twin_body = body_of_define(&ir_text, &["append", ".$consume$"]);
    assert_contains(twin_body, "in_place");
    assert_contains(twin_body, "grow");
    // The grow path relocates through the allocator funnel and
    // releases the old buffer.
    assert_contains(twin_body, "call ptr @koja_alloc");
    assert_contains(twin_body, "call void @koja_free");
}

#[test]
fn hashtable_twins_skip_the_copy_on_write_clone() {
    let source = "
        m: Map<Int, Int> = Map.new()
        m = m.put(1, 2)
        s: Set<Int> = Set.new()
        s = s.insert(3)
        (m.length() + s.length()).print()
        ";

    let ir_text = emit(source);
    for method in ["put", "insert"] {
        let twin_body = body_of_define(&ir_text, &[method, ".$consume$"]);
        assert!(
            !twin_body.contains("cow_entries"),
            "the consuming `{method}` twin must not clone the receiver's buffers:\n{twin_body}",
        );
        // The load-factor resize stays, since a full table still
        // relocates before inserting.
        assert_contains(twin_body, "need_resize");
    }
}
