use koja_ir::{IRInstruction, IRType};

mod common;

use common::{all_instructions, lower_script_source as lower};

#[test]
fn set_operations_lower_to_concrete_method_calls() {
    let script = lower(
        "
        set: Set<Int> = Set.new()
        set = set.insert(1)
        set.has?(1)
        ",
    );
    let calls: Vec<_> = all_instructions(&script.blocks)
        .filter_map(|instruction| match instruction {
            IRInstruction::Call { callee, .. } => Some(callee.mangled()),
            _ => None,
        })
        .collect();

    assert!(calls.iter().any(|callee| callee.contains(".has?/2")));
    // The rebind's receiver dies at the call, so consume fusion
    // rewrites the insert to the buffer-consuming twin.
    assert!(
        calls
            .iter()
            .any(|callee| callee.contains(".insert/2.$consume$"))
    );
    assert!(calls.iter().any(|callee| callee.contains(".new/0")));
    assert_eq!(script.return_type, IRType::Bool);
}
