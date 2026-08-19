use koja_ast::util::dedent;
use koja_ir_llvm::emit_script_llvm_ir;

mod common;

use common::{APP_NAME, extract_function_body, lower_script_source as lower};

#[test]
fn recursive_enum_drop_glue_frees_each_indirect_payload_box() {
    let source = "
        enum Tree
          Leaf(Int)
          Branch(Tree, Tree)
        end

        fn discard(tree: Tree)
          ()
        end

        1
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");
    let drop_body = extract_function_body(&ir_text, "\"TestApp.Tree.$drop$\"");

    assert!(
        drop_body.contains("icmp ne ptr"),
        "recursive payload drop should guard null boxes:\n{drop_body}",
    );
    assert_eq!(
        drop_body.matches("call void @koja_free").count(),
        2,
        "recursive Branch drop should free both payload boxes when uniquely owned:\n{drop_body}",
    );
    assert_eq!(
        drop_body.matches("call void @koja_rc_dec").count(),
        2,
        "recursive Branch drop should decrement shared payload boxes:\n{drop_body}",
    );
}

#[test]
fn recursive_enum_clone_glue_shares_payload_boxes() {
    let source = "
        enum Tree
          Leaf(Int)
          Branch(Tree, Tree)
        end

        fn discard(tree: Tree)
          ()
        end

        1
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");
    let clone_body = extract_function_body(&ir_text, "\"TestApp.Tree.$clone$\"");

    assert_eq!(
        clone_body.matches("call void @koja_rc_inc").count(),
        2,
        "recursive Branch clone should share both payload boxes via rc++:\n{clone_body}",
    );
    assert!(
        !clone_body.contains("TestApp.Tree.$clone$\"("),
        "recursive Branch clone should not recurse into payload copies:\n{clone_body}",
    );
    assert!(
        !clone_body.contains("call ptr @koja_alloc"),
        "recursive Branch clone should not allocate fresh boxes:\n{clone_body}",
    );
}

#[test]
fn recursive_struct_field_overwrite_frees_replaced_box() {
    let source = "
        struct Node
          next: Node
        end

        fn replace(node: Node, next: Node) -> Node
          node.next = next
          node
        end

        1
        ";

    let script = lower(&dedent(source));
    let ir_text =
        emit_script_llvm_ir(&script, APP_NAME).expect("emit_script_llvm_ir should succeed");
    let replace_body = extract_function_body(&ir_text, "TestApp.replace");
    let drop_body = extract_function_body(&ir_text, "\"TestApp.Node.$drop$\"");

    assert!(
        replace_body.contains("call void @koja_free"),
        "recursive field overwrite should free the replaced box when uniquely owned:\n{replace_body}",
    );
    assert!(
        replace_body.contains("call void @koja_rc_dec"),
        "recursive field overwrite should decrement a shared replaced box:\n{replace_body}",
    );
    assert!(
        drop_body.contains("icmp ne ptr"),
        "recursive field drop should guard null boxes:\n{drop_body}",
    );
}
