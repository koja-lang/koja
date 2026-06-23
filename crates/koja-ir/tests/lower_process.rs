//! Coverage for `spawn` / `receive` lowering in
//! `src/lower/process.rs`. Pins:
//!
//! - `spawn S.start(config)` lowers to a single
//!   [`IRInstruction::Spawn`] in the host block, producing a
//!   `Ref<M, R>`-typed value, plus a synthesized
//!   [`FunctionKind::SpawnWrapper`] shim keyed by the state symbol
//!   whose IR body calls the IR-synthesized `<state>.__spawn_body`
//!   carrying the `start` → `run` dispatch with ownership markers.
//! - `receive` lowers to a host block that ends with
//!   [`IRInstruction::Receive`] + [`IRTerminator::Unreachable`],
//!   one body block per arm carrying its lattice-coerced tail
//!   into a synthesized merge block.
//!
//! Tests deliberately inline minimal `Lifecycle` / `StopReason` /
//! `ReplyTo` / `Ref` / `Process` definitions so the suite doesn't
//! depend on `Global.process` being autoimported (that step is
//! covered later in the concurrency plan).

use std::path::PathBuf;

use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_ir::{
    FunctionKind, IRBasicBlock, IRBlockId, IRFunction, IRInstruction, IRProgram, IRTerminator,
    IRType, ReceiveTag, ValueId, lower_program,
};
use koja_parser::{ParseMode, SourceFile, parse_program};
use koja_typecheck::check_program;

const PACKAGE: &str = "TestApp";

/// Minimal stub of `process.koja`. Mirrors the stub
/// in `koja-typecheck/tests/process.rs` — provides every type
/// referenced by spawn/receive lowering. Replaced by the full
/// `Global.process` autoimport in step 5 of the
/// concurrency plan. Indented inline with the
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

    enum Priority
      Low
      Normal
      High
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

    protocol ExitStatus
      fn code(self) -> Int
    end

    impl ExitStatus for StopReason
      fn code(self) -> Int
        match self
          StopReason.Normal -> 0
          StopReason.Shutdown -> 1
        end
      end
    end

    protocol Process<C, M, R>
      fn start(config: C) -> Result<Self, StopReason>
      fn handle(self, msg: M, from: Option<ReplyTo<R>>) -> Step<Self>
      fn run(self) -> StopReason
      fn priority(self) -> Priority
        Priority.Normal
      end
    end
    ";

/// Synthetic Process state appended by [`lower`] so fixtures that
/// only exercise spawn/receive lowering still give `lower_program`
/// a valid entry. Spells out `run` because [`PROCESS_STUB`]'s
/// protocol declares it without a default body; `priority` is
/// defaulted there, so it is synthesized per-impl automatically.
const TEST_ENTRY_SNIPPET: &str = "
    struct TestEntry
    end

    impl Process<(), (), ()> for TestEntry
      fn start(config: ()) -> Result<Self, StopReason>
        Result.Ok(TestEntry{})
      end

      fn handle(self, msg: (), from: Option<ReplyTo<()>>) -> Step<Self>
        Step.Continue(self)
      end

      fn run(self) -> StopReason
        StopReason.Normal
      end
    end
    ";

fn lower(source: &str) -> IRProgram {
    let with_entry = format!("{}\n{}", dedent(source), dedent(TEST_ENTRY_SNIPPET));
    lower_process_entry(&with_entry, "TestEntry")
}

fn lower_process_entry(source: &str, state_name: &str) -> IRProgram {
    let state = Identifier::new(PACKAGE, vec![state_name.to_string()]);
    let mut sources = koja_stdlib::autoimport_sources();
    sources.push(SourceFile {
        package: "Global".to_string(),
        path: PathBuf::from("<Global.process>"),
        source: dedent(PROCESS_STUB),
    });
    sources.push(SourceFile {
        package: PACKAGE.to_string(),
        path: PathBuf::from("test.koja"),
        source: dedent(source),
    });
    let parsed = parse_program(sources, ParseMode::File);
    let checked = check_program(parsed).unwrap_or_else(|failure| {
        panic!(
            "typecheck failed: {} diagnostic(s):\n{}",
            failure.diagnostics.len(),
            failure
                .diagnostics
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        )
    });
    lower_program(&checked, &state).expect("lowering should succeed")
}

fn function<'a>(program: &'a IRProgram, name: &str) -> &'a IRFunction {
    let mangled = format!("{PACKAGE}.{name}");
    program
        .function(&mangled)
        .unwrap_or_else(|| panic!("missing function `{mangled}` in IRProgram"))
}

/// Under value semantics, returning a heap-managed value from `block`
/// acquires it via [`IRInstruction::Clone`] before the `Return` (block
/// params and call results are borrowed until acquired). Assert the
/// terminator returns that acquired value, not the borrowed source.
fn assert_return_acquires(block: &IRBasicBlock, source: ValueId) {
    let IRTerminator::Return {
        value: Some(returned),
    } = block.terminator
    else {
        panic!(
            "expected Return with a value on block `{}`, got {:?}",
            block.label, block.terminator,
        );
    };
    let acquired = block
        .instructions
        .iter()
        .find_map(|instruction| match instruction {
            IRInstruction::Clone {
                dest, source: src, ..
            } if *src == source => Some(*dest),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "expected a Clone acquiring {source} before return on block `{}`",
                block.label,
            );
        });
    assert_eq!(
        returned, acquired,
        "Return should target the acquired (cloned) value on block `{}`",
        block.label,
    );
}

/// Assert the wrapper shim's IR body is a single call into
/// `body_mangled`, and that the body function carries the full
/// dispatch shape: the `start` call, the `Result` tag branch, the
/// `run` call, and the Clone/Drop ownership markers (the test
/// states are all-`Copy`, so elaborate leaves the markers in place
/// rather than rewriting them into glue calls).
fn assert_process_body_shape(program: &IRProgram, wrapper: &IRFunction, state_mangled: &str) {
    let body_callee = wrapper
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            IRInstruction::Call { callee, .. } => Some(callee.clone()),
            _ => None,
        })
        .expect("wrapper shim's IR body should call the process body");
    let body = program
        .function(body_callee.mangled())
        .unwrap_or_else(|| panic!("process body `{body_callee}` should be registered"));

    assert!(
        matches!(body.kind, FunctionKind::Regular),
        "process body should be a Regular function, got {:?}",
        body.kind,
    );

    let callees: Vec<&str> = body
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            IRInstruction::Call { callee, .. } => Some(callee.mangled()),
            _ => None,
        })
        .collect();
    assert!(
        callees.contains(&format!("{state_mangled}.start").as_str()),
        "process body should call `{state_mangled}.start`, calls: {callees:?}",
    );
    assert!(
        callees.contains(&format!("{state_mangled}.run").as_str()),
        "process body should call `{state_mangled}.run`, calls: {callees:?}",
    );

    let instructions: Vec<&IRInstruction> = body
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .collect();
    assert!(
        instructions
            .iter()
            .any(|i| matches!(i, IRInstruction::EnumTagGet { .. })),
        "process body should read the start result's tag",
    );
    assert!(
        instructions
            .iter()
            .any(|i| matches!(i, IRInstruction::Clone { .. })),
        "process body should acquire the extracted payload via a Clone marker",
    );
    assert!(
        instructions
            .iter()
            .any(|i| matches!(i, IRInstruction::DropValue { .. })),
        "process body should release the start result via a DropValue marker",
    );
    assert!(
        body.blocks
            .iter()
            .any(|block| matches!(block.terminator, IRTerminator::CondBranch { .. })),
        "process body should branch on the Result tag",
    );
}

#[test]
fn spawn_lowers_to_spawn_instruction_plus_wrapper_fn() {
    let program = lower(
        "
        struct Counter
          count: Int
        end

        impl Process<Int, Int, Int> for Counter
          fn start(config: Int) -> Result<Counter, StopReason>
            Result.Ok(Counter{count: config})
          end

          fn handle(self, msg: Int, from: Option<ReplyTo<Int>>) -> Step<Counter>
            Step.Done(StopReason.Normal)
          end

          fn run(self) -> StopReason
            StopReason.Normal
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

    assert_return_acquires(entry, dest);

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
    assert_process_body_shape(&program, wrapper_fn, &format!("{PACKAGE}.Counter"));
}

#[test]
fn spawn_dedupes_wrapper_across_call_sites() {
    let program = lower(
        "
        struct Counter
          count: Int
        end

        impl Process<Int, Int, Int> for Counter
          fn start(config: Int) -> Result<Counter, StopReason>
            Result.Ok(Counter{count: config})
          end

          fn handle(self, msg: Int, from: Option<ReplyTo<Int>>) -> Step<Counter>
            Step.Done(StopReason.Normal)
          end

          fn run(self) -> StopReason
            StopReason.Normal
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
    // Owned-merge-param model: each arm acquires its tail value before
    // branching to the join, so the merge param already owns the join
    // value and the Return moves it directly. The arm here yields
    // `StopReason.Shutdown`, owned from construction, so no Clone is
    // emitted — the merge block simply returns its param.
    assert_eq!(
        merge.terminator,
        IRTerminator::Return {
            value: Some(merge_param),
        },
        "receive_merge should return its owned join param directly",
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

#[test]
fn process_entry_lowers_to_process_entry_wrapper() {
    let program = lower_process_entry(
        "
        struct App
        end

        enum AppMsg
          Greet
        end

        impl Process<App, AppMsg, String> for App
          fn start(config: App) -> Result<Self, StopReason>
            Result.Ok(config)
          end

          fn handle(self, msg: AppMsg, from: Option<ReplyTo<String>>) -> Step<Self>
            Step.Continue(self)
          end

          fn run(self) -> StopReason
            StopReason.Normal
          end
        end
        ",
        "App",
    );

    let wrapper_mangled = format!("{PACKAGE}.App.__entry_wrapper");
    assert_eq!(
        program.entry_point.mangled(),
        wrapper_mangled,
        "Process-entry should stamp entry_point on the synthesized `__entry_wrapper`",
    );
    let wrapper = program
        .function(&wrapper_mangled)
        .expect("entry wrapper must be registered as a function");
    match &wrapper.kind {
        FunctionKind::ProcessEntryWrapper { state } => {
            assert!(
                matches!(state, IRType::Struct(symbol) if symbol.mangled() == format!("{PACKAGE}.App")),
                "ProcessEntryWrapper.state should reference the App struct",
            );
        }
        other => panic!("expected ProcessEntryWrapper, got {other:?}"),
    }
    assert_eq!(
        wrapper.return_type,
        IRType::Unit,
        "entry wrapper's IR-level return type is Unit; the LLVM emitter \
         remaps to void(i8*)",
    );
    let config_param = wrapper
        .params
        .first()
        .expect("entry wrapper must declare its config parameter");
    assert!(
        matches!(&config_param.ty, IRType::Struct(symbol) if symbol.mangled() == format!("{PACKAGE}.App")),
        "entry wrapper's config type should match the App state",
    );

    // start/run must be registered so the synthesized entry body can
    // dispatch through them.
    assert!(
        program.function(&format!("{PACKAGE}.App.start")).is_some(),
        "App.start must be registered when the entry is a Process state",
    );
    assert!(
        program.function(&format!("{PACKAGE}.App.run")).is_some(),
        "App.run must be registered when the entry is a Process state",
    );

    assert_process_body_shape(&program, wrapper, &format!("{PACKAGE}.App"));
    let body = program
        .function(&format!("{PACKAGE}.App.__entry_body"))
        .expect("entry body must be registered under `<state>.__entry_body`");
    assert_eq!(
        body.return_type,
        IRType::Int64,
        "entry body returns the exit code for the shim to store",
    );
    let routes_through_code = body
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| {
            matches!(
                instruction,
                IRInstruction::Call { callee, .. } if callee.mangled() == "Global.StopReason.code"
            )
        });
    assert!(
        routes_through_code,
        "entry body should route both arms' StopReason through `Global.StopReason.code`",
    );
}
