use koja_ir::{IRInstruction, IRType};

mod common;

use common::{all_instructions, lower_script_source as lower};

#[test]
fn map_literal_lowers_to_new_and_put_chain() {
    let script = lower("[\"a\": 1, \"b\": 2]\n");
    let calls: Vec<_> = all_instructions(&script.blocks)
        .filter_map(|instruction| match instruction {
            IRInstruction::Call { callee, .. } => Some(callee.mangled()),
            _ => None,
        })
        .collect();

    assert_eq!(
        calls
            .iter()
            .filter(|callee| callee.contains(".Map") && callee.ends_with(".new"))
            .count(),
        1,
    );
    // Each put's receiver is an owned temp that dies at the call, so
    // consume fusion rewrites the chain to the buffer-consuming twin.
    assert_eq!(
        calls
            .iter()
            .filter(|callee| callee.contains(".Map") && callee.ends_with(".put.$consume$"))
            .count(),
        2,
    );
    assert!(
        calls.iter().all(|callee| !callee.contains(".List")),
        "default map literals must not allocate an entry list: {calls:#?}",
    );
    assert!(matches!(script.return_type, IRType::Map { .. }));
}
