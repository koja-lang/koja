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
        "recursive Branch drop should free both payload boxes:\n{drop_body}",
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
        "recursive field overwrite should free the replaced box:\n{replace_body}",
    );
    assert!(
        drop_body.contains("icmp ne ptr"),
        "recursive field drop should guard null boxes:\n{drop_body}",
    );
}
