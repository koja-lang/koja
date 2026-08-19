//! Consume-fusion regressions (`elaborate::consume`). Pins that a
//! `List.append` / `Map.put` / `Set.insert` call whose receiver value
//! dies at the call site is rewritten to its `.$consume$` twin with
//! the death deleted, that the twin registers as an empty-bodied
//! `Consuming` intrinsic next to the original, and that a receiver
//! that stays live keeps the copying intrinsic.

use koja_ir::{
    ConsumingMethod, FunctionKind, IRBasicBlock, IRInstruction, IRIntrinsicId, IRScript,
};

mod common;

use common::{all_instructions, lower_script_source as lower, script_function};

/// Mangled callee names of every `Call` across `blocks`.
fn callees(blocks: &[IRBasicBlock]) -> Vec<&str> {
    all_instructions(blocks)
        .filter_map(|instruction| match instruction {
            IRInstruction::Call { callee, .. } => Some(callee.mangled()),
            _ => None,
        })
        .collect()
}

/// Callees of consuming-twin calls (`.$consume$` symbols) across
/// `blocks`.
fn consuming_callees(blocks: &[IRBasicBlock]) -> Vec<&str> {
    callees(blocks)
        .into_iter()
        .filter(|callee| callee.contains(".$consume$"))
        .collect()
}

/// Copying-intrinsic calls to `method` (an unfused `append` / `put` /
/// `insert`) across `blocks`.
fn copying_calls(blocks: &[IRBasicBlock], method: &str) -> usize {
    callees(blocks)
        .into_iter()
        .filter(|callee| callee.contains(method) && !callee.contains(".$consume$"))
        .count()
}

/// The registered twin function whose symbol contains `needle`,
/// asserted to be an empty-bodied `Consuming` intrinsic for `method`.
fn assert_twin_registered(script: &IRScript, needle: &str, method: ConsumingMethod) {
    let twin = script
        .packages
        .iter()
        .flat_map(|package| package.functions.values())
        .find(|function| {
            let mangled = function.symbol.mangled();
            mangled.contains(needle) && mangled.contains(".$consume$")
        })
        .unwrap_or_else(|| panic!("no `.$consume$` twin containing `{needle}` was registered"));
    assert!(
        matches!(
            twin.kind,
            FunctionKind::Intrinsic(IRIntrinsicId::Consuming(m)) if m == method
        ),
        "twin `{}` should be a Consuming({method:?}) intrinsic, got {:?}",
        twin.symbol,
        twin.kind,
    );
    assert!(
        twin.blocks.is_empty(),
        "twin `{}` should have an empty body (backends synthesize it)",
        twin.symbol,
    );
    let original = script
        .function(twin.symbol.mangled().trim_end_matches(".$consume$"))
        .expect("twin's copying original should stay registered");
    assert_eq!(
        twin.params.len(),
        original.params.len(),
        "twin `{}` should keep the original's signature",
        twin.symbol,
    );
}

#[test]
fn rebind_loop_fuses_append_into_consuming_twin() {
    let source = "
        fn build(n: Int) -> List<Int>
          xs: List<Int> = []
          i = 0
          while i < n
            xs = xs.append(i)
            i += 1
          end
          xs
        end

        build(3).length()
    ";

    let script = lower(source);
    let blocks = &script_function(&script, "build").blocks;
    assert_eq!(
        consuming_callees(blocks).len(),
        1,
        "the rebind loop's append should fuse",
    );
    assert_eq!(
        copying_calls(blocks, "append"),
        0,
        "no copying append call should remain in the loop",
    );
    assert_twin_registered(&script, "append", ConsumingMethod::ListAppend);
}

#[test]
fn fused_rebind_deletes_the_stale_read_and_drop() {
    let source = "
        fn push(xs: List<Int>, item: Int) -> List<Int>
          out = xs.slice(0, xs.length())
          out = out.append(item)
          out
        end

        push([1], 2).length()
    ";

    let script = lower(source);
    let blocks = &script_function(&script, "push").blocks;
    let consuming = consuming_callees(blocks);
    assert_eq!(consuming.len(), 1, "the rebind append should fuse");

    // The receiver's death was fused away, so no `DropValue` of a
    // list-typed value may remain between the consuming call and the
    // slot write (the whole trio collapsed to call + write).
    for block in &script_function(&script, "push").blocks {
        let Some(call_index) = block.instructions.iter().position(|instruction| {
            matches!(
                instruction,
                IRInstruction::Call { callee, .. } if callee.mangled().contains(".$consume$")
            )
        }) else {
            continue;
        };
        assert!(
            matches!(
                block.instructions.get(call_index + 1),
                Some(IRInstruction::LocalWrite { .. })
            ),
            "a fused rebind should be followed directly by the slot write",
        );
    }
}

#[test]
fn owned_temp_chain_fuses_second_append_only() {
    let source = "
        fn chain(seed: List<Int>) -> List<Int>
          seed.append(1).append(2)
        end

        chain([]).length()
    ";

    let script = lower(source);
    let blocks = &script_function(&script, "chain").blocks;
    assert_eq!(
        consuming_callees(blocks).len(),
        1,
        "the owned intermediate's append should fuse",
    );
    assert_eq!(
        copying_calls(blocks, "append"),
        1,
        "the borrowed `seed` receiver must keep the copying append",
    );
}

#[test]
fn live_receiver_keeps_the_copying_intrinsic() {
    let source = "
        fn keep(xs: List<Int>) -> Int
          ys = xs.append(1)
          ys.length() + xs.length()
        end

        keep([2])
    ";

    let script = lower(source);
    let blocks = &script_function(&script, "keep").blocks;
    assert!(
        consuming_callees(blocks).is_empty(),
        "a receiver that stays live must not fuse",
    );
    assert_eq!(copying_calls(blocks, "append"), 1);
}

#[test]
fn map_put_and_set_insert_rebinds_fuse() {
    let source = "
        fn tally(n: Int) -> Int
          m: Map<Int, Int> = Map.new()
          m = m.put(n, n * 2)
          s: Set<Int> = Set.new()
          s = s.insert(n)
          m.length() + s.length()
        end

        tally(4)
    ";

    let script = lower(source);
    let blocks = &script_function(&script, "tally").blocks;
    assert_eq!(
        consuming_callees(blocks).len(),
        2,
        "both the map put and the set insert rebinds should fuse",
    );
    assert_twin_registered(&script, "put", ConsumingMethod::MapPut);
    assert_twin_registered(&script, "insert", ConsumingMethod::SetInsert);
}

#[test]
fn alias_taken_before_the_rebind_still_fuses() {
    // `b = a` deep-clones, so consuming `a`'s buffer afterward is
    // safe and the fusion must not be blocked by the earlier alias.
    let source = "
        fn split(seed: Int) -> Int
          a: List<Int> = [seed]
          b = a
          a = a.append(seed)
          a.length() + b.length()
        end

        split(7)
    ";

    let script = lower(source);
    let blocks = &script_function(&script, "split").blocks;
    // Two fused sites, as the `[seed]` literal's construction append
    // is an owned temp and fuses as well.
    assert_eq!(
        consuming_callees(blocks).len(),
        2,
        "the rebind should fuse even with an earlier alias of the slot",
    );
    assert_eq!(copying_calls(blocks, "append"), 0);
}
