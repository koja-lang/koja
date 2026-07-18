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
    assert_eq!(
        calls
            .iter()
            .filter(|callee| callee.contains(".Map") && callee.ends_with(".put"))
            .count(),
        2,
    );
    assert!(matches!(script.return_type, IRType::Map { .. }));
}
