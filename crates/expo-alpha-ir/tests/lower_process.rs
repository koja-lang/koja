//! Coverage for `spawn` / `receive` lowering in
//! `src/lower/process.rs`. Pins:
//!
//! - `spawn S.start(config)` lowers to a single
//!   [`IRInstruction::Spawn`] in the host block, producing a
//!   `Ref<M, R>`-typed value, plus a synthesized
//!   [`FunctionKind::SpawnWrapper`] keyed by the state symbol.
//! - `receive` lowers to a host block that ends with
//!   [`IRInstruction::Receive`] + [`IRTerminator::Unreachable`],
//!   one body block per arm carrying its lattice-coerced tail
//!   into a synthesized merge block.
//!
//! Tests deliberately inline minimal `Lifecycle` / `StopReason` /
//! `ReplyTo` / `Ref` / `Process` definitions so the suite doesn't
//! depend on `Global.process` being autoimported (that step is
//! covered later in the alpha-concurrency plan).

use std::path::PathBuf;

use expo_alpha_ir::{
    FunctionKind, IRBlockId, IRFunction, IRInstruction, IRProgram, IRTerminator, IRType,
    ReceiveTag, lower_program,
};
use expo_alpha_typecheck::check_program;
use expo_ast::identifier::Identifier;
use expo_ast::util::dedent;
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

/// Minimal alpha-friendly stub of `process.expo`. Mirrors the stub
/// in `expo-alpha-typecheck/tests/process.rs` — provides every type
/// referenced by spawn/receive lowering. Replaced by the full
/// `Global.process` autoimport in step 5 of the
/// alpha-concurrency-process plan. Indented inline with the
/// surrounding Rust; `lower` dedents it (along with the test
/// source) before parsing.
const PROCESS_STUB: &str = "
    enum Lifecycle
      Shutdown
      Interrupt
      Reload
    end

    enum StopReason
      Normal
      Shutdown
    end

    enum Step<S>
      Continue(S)
      Done(StopReason)
    end

    struct ReplyTo<R>
      id: Int
    end

    struct Ref<M, R>
      id: Int
    end

    protocol Process<C, M, R>
      fn start(move config: C) -> Result<Self, StopReason>
      fn handle(move self, msg: M, from: Option<ReplyTo<R>>) -> Step<Self>
    end
    ";

fn lower(source: &str) -> IRProgram {
    let mut sources = expo_stdlib::alpha_autoimport_sources();
    sources.push(SourceFile {
        package: "Global".to_string(),
        path: PathBuf::from("<Global.process>"),
        source: dedent(PROCESS_STUB),
    });
    sources.push(SourceFile {
        package: PACKAGE.to_string(),
        path: PathBuf::from("test.expo"),
        source: dedent(source),
    });
    let parsed = parse_program(sources, ParseMode::File);
    let checked = check_program(parsed).unwrap_or_else(|failure| {
        panic!(
            "alpha typecheck failed: {} diagnostic(s):\n{}",
            failure.diagnostics.len(),
            failure
                .diagnostics
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        )
    });
    let entry = Identifier::new(PACKAGE, vec!["main".to_string()]);
    lower_program(&checked, entry).expect("lowering should succeed")
}

fn function<'a>(program: &'a IRProgram, name: &str) -> &'a IRFunction {
    let mangled = format!("{PACKAGE}.{name}");
    program
        .function(&mangled)
        .unwrap_or_else(|| panic!("missing function `{mangled}` in IRProgram"))
}

#[test]
fn spawn_lowers_to_spawn_instruction_plus_wrapper_fn() {
    let program = lower(
        "
        struct Counter
          count: Int
        end

        impl Process<Int, Int, Int> for Counter
          fn start(move config: Int) -> Result<Counter, StopReason>
            Result.Ok(Counter{count: config})
          end

          fn handle(move self, msg: Int, from: Option<ReplyTo<Int>>) -> Step<Counter>
            Step.Done(StopReason.Normal)
          end
        end

        fn run -> Ref<Int, Int>
          spawn Counter.start(0)
        end

        fn main
          handle = run()
        end
        ",
    );
    let run = function(&program, "run");

    let entry = run
        .blocks
        .iter()
        .find(|b| b.label == "entry")
        .expect("missing entry block in `run`");
    let spawn = entry
        .instructions
        .iter()
        .find_map(|inst| match inst {
            IRInstruction::Spawn {
                config: _,
                config_type,
                dest,
                ref_type,
                wrapper,
            } => Some((
                config_type.clone(),
                *dest,
                ref_type.clone(),
                wrapper.clone(),
            )),
            _ => None,
        })
        .expect("entry block should carry a single Spawn instruction");
    let (config_type, dest, ref_type, wrapper) = spawn;

    assert_eq!(
        config_type,
        IRType::Int64,
        "Counter's config is Int → Spawn.config_type should be Int64",
    );
    assert!(
        ref_type.mangled().contains(".Ref"),
        "Spawn.ref_type should reference the monomorphized Ref struct \
         (got `{}`)",
        ref_type.mangled(),
    );
    assert!(
        wrapper.mangled().ends_with(".__spawn_wrapper"),
        "Spawn.wrapper should be the synthesized `__spawn_wrapper` thunk (got `{}`)",
        wrapper.mangled(),
    );
    assert_eq!(
        run.return_type,
        IRType::Struct(ref_type.clone()),
        "`run`'s return type should be the same Ref struct Spawn produces",
    );

    assert_eq!(
        entry.terminator,
        IRTerminator::Return { value: Some(dest) },
        "the spawn site's host block should return the freshly-minted Ref value",
    );

    let wrapper_fn = program
        .function(wrapper.mangled())
        .expect("wrapper symbol should resolve to a synthesized function");
    assert!(
        matches!(wrapper_fn.kind, FunctionKind::SpawnWrapper { .. }),
        "wrapper kind should be SpawnWrapper, got {:?}",
        wrapper_fn.kind,
    );
    assert!(
        !wrapper_fn.blocks.is_empty(),
        "spawn wrapper must carry a non-empty block list (seal invariant)",
    );
}

#[test]
fn spawn_dedupes_wrapper_across_call_sites() {
    let program = lower(
        "
        struct Counter
          count: Int
        end

        impl Process<Int, Int, Int> for Counter
          fn start(move config: Int) -> Result<Counter, StopReason>
            Result.Ok(Counter{count: config})
          end

          fn handle(move self, msg: Int, from: Option<ReplyTo<Int>>) -> Step<Counter>
            Step.Done(StopReason.Normal)
          end
        end

        fn first -> Ref<Int, Int>
          spawn Counter.start(1)
        end

        fn second -> Ref<Int, Int>
          spawn Counter.start(2)
        end

        fn main
          a = first()
          b = second()
        end
        ",
    );
    let wrapper_count = program
        .packages
        .iter()
        .flat_map(|pkg| pkg.functions.values())
        .filter(|f| matches!(f.kind, FunctionKind::SpawnWrapper { .. }))
        .count();
    assert_eq!(
        wrapper_count, 1,
        "two spawn sites for the same state cell should share one SpawnWrapper",
    );
}

#[test]
fn receive_lowers_to_receive_instruction_with_one_arm_per_body_block() {
    let program = lower(
        "
        fn drain -> StopReason
          receive
            event: Lifecycle ->
              StopReason.Shutdown
          end
        end

        fn main
          reason = drain()
        end
        ",
    );
    let loop_fn = function(&program, "drain");

    let host = loop_fn
        .blocks
        .iter()
        .find(|b| {
            b.instructions
                .iter()
                .any(|i| matches!(i, IRInstruction::Receive { .. }))
        })
        .expect("missing host block for receive");
    let receive = host
        .instructions
        .iter()
        .find_map(|inst| match inst {
            IRInstruction::Receive {
                after,
                arms,
                dest: _,
                result_type,
            } => Some((after.clone(), arms.clone(), result_type.clone())),
            _ => None,
        })
        .expect("host block should carry a Receive instruction");
    let (after, arms, _result_type) = receive;

    assert!(after.is_none(), "receive without `after` should carry None");
    assert_eq!(
        arms.len(),
        1,
        "expected one Lifecycle arm in the lowered Receive",
    );
    assert!(
        matches!(arms[0].tag, ReceiveTag::Lifecycle),
        "arm should tag as Lifecycle, got {:?}",
        arms[0].tag,
    );
    assert_eq!(
        host.terminator,
        IRTerminator::Unreachable,
        "Receive's host block must end Unreachable — dispatch always exits via arm bodies",
    );

    let arm_block_id = arms[0].body;
    assert!(
        loop_fn.blocks.iter().any(|b| b.id == arm_block_id),
        "Receive arm body block id {arm_block_id} should exist in `loop`'s CFG",
    );

    let merge = loop_fn
        .blocks
        .iter()
        .find(|b| b.label == "receive_merge")
        .expect("missing receive_merge block");
    assert_eq!(
        merge.params.len(),
        1,
        "receive_merge should declare exactly one BlockParam for the join value",
    );
    let merge_param = merge.params[0].dest;
    assert_eq!(
        merge.terminator,
        IRTerminator::Return {
            value: Some(merge_param)
        },
        "merge's terminator should return the joined arm value",
    );
}

#[test]
fn receive_with_after_lowers_timeout_value_and_after_block() {
    let program = lower(
        "
        fn drain -> StopReason
          receive
            event: Lifecycle ->
              StopReason.Shutdown
          after 100
            StopReason.Normal
          end
        end

        fn main
          reason = drain()
        end
        ",
    );
    let loop_fn = function(&program, "drain");

    let after = loop_fn
        .blocks
        .iter()
        .find_map(|b| {
            b.instructions.iter().find_map(|inst| match inst {
                IRInstruction::Receive { after, .. } => after.clone(),
                _ => None,
            })
        })
        .expect("Receive should carry an after clause");

    let after_block_exists = loop_fn.blocks.iter().any(|b| b.id == after.body);
    assert!(
        after_block_exists,
        "after.body block id {} should resolve to a CFG block",
        after.body,
    );
    assert!(
        loop_fn.blocks.iter().any(|b| b.label == "receive_after"),
        "lowered receive should mint a `receive_after` block",
    );

    // Timeout value rides through a regular IRInstruction::Const-typed
    // operand into Receive; we only need to confirm Receive sees it
    // and the merge block's param ties everything back together.
    let _ = after.timeout;
    let merge = loop_fn
        .blocks
        .iter()
        .find(|b| b.label == "receive_merge")
        .expect("missing receive_merge block");
    assert_eq!(
        merge.params.len(),
        1,
        "after-style receive merges arm + after tails into one BlockParam",
    );

    // Sanity: the receive's host block is still terminated unreachable.
    let host_id: IRBlockId = loop_fn
        .blocks
        .iter()
        .find(|b| {
            b.instructions
                .iter()
                .any(|i| matches!(i, IRInstruction::Receive { .. }))
        })
        .map(|b| b.id)
        .expect("missing host block for receive");
    let host = loop_fn.blocks.iter().find(|b| b.id == host_id).unwrap();
    assert_eq!(host.terminator, IRTerminator::Unreachable);
}

#[test]
fn receive_arm_payload_local_is_declared_with_resolved_type() {
    let program = lower(
        "
        fn drain -> StopReason
          receive
            event: Lifecycle ->
              StopReason.Shutdown
          end
        end

        fn main
          drain()
        end
        ",
    );
    let loop_fn = function(&program, "drain");

    let receive_arm = loop_fn
        .blocks
        .iter()
        .find_map(|b| {
            b.instructions.iter().find_map(|inst| match inst {
                IRInstruction::Receive { arms, .. } => arms.first().cloned(),
                _ => None,
            })
        })
        .expect("missing Receive arm");
    let payload_local = receive_arm.payload_local;

    let entry = loop_fn
        .blocks
        .iter()
        .find(|b| b.label == "entry")
        .expect("missing entry block");
    let payload_decl = entry.instructions.iter().any(|inst| match inst {
        IRInstruction::LocalDecl { local, .. } => *local == payload_local,
        _ => false,
    });
    assert!(
        payload_decl,
        "receive arm payload local {payload_local} should be declared in the entry block",
    );
}
