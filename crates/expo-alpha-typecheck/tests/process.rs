//! Phase 4 typecheck coverage for `spawn` / `receive` and the
//! `Global.process` surface. Pins the contract:
//!
//! - `spawn Type.start(config)` resolves to `Ref<M, R>` picked off
//!   the receiver's `impl Process<C, M, R>`. Non-`start` callees,
//!   non-method-call inner expressions, and receivers without a
//!   `Process` impl all diagnose.
//! - `receive` arms must use a `Pattern::TypedBinding` whose
//!   annotation is either a business envelope
//!   (`Pair<M, Option<ReplyTo<R>>>`) or `Lifecycle`. Arm bodies +
//!   the `after` body join under the same lattice as `match`.
//! - `receive after timeout body end` requires `Int` for the
//!   timeout (no arrow on the `after` clause; body follows directly).
//! - The receive arm binding stamps a `LocalId` on the typed
//!   binding so IR lower can reach it.
//!
//! `Global.process` is not yet in `ALPHA_AUTOIMPORT`; tests prepend
//! a minimal stub that declares the surface this slice cares about
//! (`Lifecycle`, `StopReason`, `Step`, `ReplyTo`, `Ref`, and the
//! `Process<C, M, R>` protocol) until step 5 of the
//! alpha-concurrency-process plan flips the autoimport switch on the
//! full `process.expo`. The full file pulls in shapes the alpha
//! pipeline doesn't support yet (`self.work()` field-as-callee in
//! `Task<R>::run`); the stub keeps us focused on the spawn/receive
//! surface this slice owns.

use std::path::PathBuf;

use expo_alpha_typecheck::{CheckFailure, CheckedProgram, check_program};
use expo_ast::ast::{ExprKind, Function, Item, Pattern, Statement};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::util::dedent;
use expo_parser::{ParseMode, SourceFile, parse_program};

const PACKAGE: &str = "TestApp";

/// Minimal alpha-friendly stub of `process.expo`. Provides every
/// type referenced in this slice's spawn/receive surface; the full
/// `process.expo` is pulled in via `ALPHA_AUTOIMPORT` after step 5.
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

enum CallError
  Timeout
  ProcessDown
end

impl Ref<M, R>
  @intrinsic
  fn call(self, msg: M, timeout: Int) -> Result<R, CallError>

  @intrinsic
  fn cast(self, msg: M)
end

protocol Process<C, M, R>
  fn start(move config: C) -> Result<Self, StopReason>
  fn handle(move self, msg: M, from: Option<ReplyTo<R>>) -> Step<Self>
end
";

fn typecheck(source: &str) -> CheckedProgram {
    parse_and_check(source).unwrap_or_else(|failure| {
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
    })
}

fn typecheck_fail(source: &str) -> CheckFailure {
    parse_and_check(source).expect_err("expected alpha typecheck to fail")
}

fn parse_and_check(source: &str) -> Result<CheckedProgram, CheckFailure> {
    let mut sources = expo_stdlib::alpha_autoimport_sources();
    sources.push(SourceFile {
        package: "Global".to_string(),
        path: PathBuf::from("<Global.process>"),
        source: PROCESS_STUB.to_string(),
    });
    sources.push(SourceFile {
        package: PACKAGE.to_string(),
        path: PathBuf::from("test.expo"),
        source: source.to_string(),
    });
    let parsed = parse_program(sources, ParseMode::File);
    check_program(parsed)
}

fn diagnostic_messages(failure: &CheckFailure) -> Vec<String> {
    failure
        .diagnostics
        .iter()
        .map(|d| d.message.clone())
        .collect()
}

fn main_fn(checked: &CheckedProgram) -> &Function {
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("test package missing");
    let file = pkg.files.first().expect("package has no files");
    file.items
        .iter()
        .find_map(|item| match item {
            Item::Function(function) if function.name == "main" => Some(function),
            _ => None,
        })
        .expect("`fn main` missing")
}

fn trailing_resolution(checked: &CheckedProgram) -> ResolvedType {
    let body = main_fn(checked).body.as_deref().expect("main body");
    let trailing = body.last().expect("non-empty body");
    match trailing {
        Statement::Expr(expr) => expr.resolution.clone(),
        other => panic!("expected trailing Statement::Expr, got {other:?}"),
    }
}

fn global_id(checked: &CheckedProgram, name: &str) -> expo_ast::identifier::GlobalRegistryId {
    checked
        .registry
        .lookup(&Identifier::new("Global", vec![name.to_string()]))
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("`Global.{name}` missing from registry"))
}

#[test]
fn spawn_start_resolves_to_ref_of_msg_and_reply() {
    // `Counter` implements `Process<Int, CounterMsg, Int>` so
    // `spawn Counter.start(0)` types as `Ref<CounterMsg, Int>`.
    let source = "
        enum CounterMsg
          Bump
          Reset
        end

        struct Counter
          count: Int
        end

        impl Process<Int, CounterMsg, Int> for Counter
          fn start(move config: Int) -> Result<Counter, StopReason>
            Result.Ok(Counter{count: config})
          end

          fn handle(move self, msg: CounterMsg, from: Option<ReplyTo<Int>>) -> Step<Self>
            Step.Continue(self)
          end
        end

        fn main
          spawn Counter.start(0)
        end
        ";
    let checked = typecheck(&dedent(source));
    let resolved = trailing_resolution(&checked);
    let ResolvedType::Named {
        resolution: Resolution::Global(head_id),
        type_args,
    } = &resolved
    else {
        panic!("expected Ref<_, _>, got {resolved:?}");
    };
    assert_eq!(*head_id, global_id(&checked, "Ref"));
    assert_eq!(type_args.len(), 2);
    let counter_msg = match &type_args[0] {
        ResolvedType::Named {
            resolution: Resolution::Global(id),
            ..
        } => *id,
        other => panic!("expected `CounterMsg` enum, got {other:?}"),
    };
    let counter_msg_entry = checked.registry.get(counter_msg).expect("CounterMsg entry");
    assert_eq!(counter_msg_entry.identifier.last(), "CounterMsg");
    let int_id = global_id(&checked, "Int");
    let ResolvedType::Named {
        resolution: Resolution::Global(reply_id),
        ..
    } = &type_args[1]
    else {
        panic!("expected reply `Int`, got {:?}", type_args[1]);
    };
    assert_eq!(*reply_id, int_id);
}

#[test]
fn spawn_inner_must_be_method_call_to_start() {
    // `spawn 1` is a shape error — there's nothing to spawn.
    let source = "
        fn main
          spawn 1
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("requires `Type.start(config)`")),
        "expected spawn shape diagnostic, got {messages:?}",
    );
}

#[test]
fn spawn_receiver_without_process_impl_diagnoses() {
    // `Bag` declares a `start` method but does not implement
    // `Process` — `spawn Bag.start(config)` must be rejected.
    let source = "
        struct Bag
          n: Int
        end

        impl Bag
          fn start(move config: Int) -> Result<Bag, StopReason>
            Result.Ok(Bag{n: config})
          end
        end

        fn main
          spawn Bag.start(0)
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("does not implement `Process`")),
        "expected `does not implement Process` diagnostic, got {messages:?}",
    );
}

#[test]
fn receive_business_arm_resolves_with_typed_binding() {
    // The arm pattern's typed-binding annotation is the arm subject
    // type. `pair: Pair<Int, Option<ReplyTo<String>>>` is a business
    // envelope; the body sees `pair` as that type.
    let source = "
        fn main -> StopReason
          receive
            pair: Pair<Int, Option<ReplyTo<String>>> ->
              StopReason.Normal
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    let resolved = trailing_resolution(&checked);
    let ResolvedType::Named {
        resolution: Resolution::Global(id),
        ..
    } = &resolved
    else {
        panic!("expected `StopReason`, got {resolved:?}");
    };
    assert_eq!(*id, global_id(&checked, "StopReason"));

    // The bound `pair` should have a `local_id` stamped on the
    // typed-binding pattern.
    let main = main_fn(&checked);
    let body = main.body.as_deref().expect("main body");
    let receive = match body.last().expect("non-empty body") {
        Statement::Expr(expr) => match &expr.kind {
            ExprKind::Receive { arms, .. } => arms,
            other => panic!("expected receive, got {other:?}"),
        },
        other => panic!("expected trailing Statement::Expr, got {other:?}"),
    };
    let arm = receive.first().expect("at least one arm");
    let Pattern::TypedBinding { local_id, .. } = &arm.pattern else {
        panic!("expected typed-binding, got {:?}", arm.pattern);
    };
    assert!(
        local_id.is_some(),
        "typed-binding `local_id` must be stamped",
    );
}

#[test]
fn receive_lifecycle_arm_resolves() {
    let source = "
        fn main -> StopReason
          receive
            event: Lifecycle ->
              StopReason.Shutdown
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    let resolved = trailing_resolution(&checked);
    let ResolvedType::Named {
        resolution: Resolution::Global(id),
        ..
    } = &resolved
    else {
        panic!("expected `StopReason`, got {resolved:?}");
    };
    assert_eq!(*id, global_id(&checked, "StopReason"));
}

#[test]
fn receive_arm_without_typed_binding_diagnoses() {
    // A regular `match`-style binding is not allowed in a receive
    // arm — the arm needs the type annotation to discriminate.
    let source = "
        fn main -> StopReason
          receive
            x ->
              StopReason.Normal
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("typed-binding pattern")),
        "expected typed-binding-required diagnostic, got {messages:?}",
    );
}

#[test]
fn receive_arm_with_unsupported_envelope_diagnoses() {
    // A typed-binding against `Int` is not a business envelope
    // (`Pair<M, Option<ReplyTo<R>>>`) and not `Lifecycle`.
    let source = "
        fn main -> StopReason
          receive
            n: Int ->
              StopReason.Normal
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("only supports business")),
        "expected envelope shape diagnostic, got {messages:?}",
    );
}

#[test]
fn receive_after_timeout_must_be_int() {
    let source = "
        fn main -> StopReason
          receive
            event: Lifecycle -> StopReason.Shutdown
          after \"slow\"
            StopReason.Normal
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("timeout must be `Int`")),
        "expected timeout-int diagnostic, got {messages:?}",
    );
}

#[test]
fn receive_arms_join_under_same_lattice_as_match() {
    // Business arm yields `StopReason`; lifecycle arm also yields
    // `StopReason`; the join is `StopReason`.
    let source = "
        fn main -> StopReason
          receive
            pair: Pair<Int, Option<ReplyTo<Int>>> -> StopReason.Normal
            event: Lifecycle -> StopReason.Shutdown
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    let resolved = trailing_resolution(&checked);
    let ResolvedType::Named {
        resolution: Resolution::Global(id),
        ..
    } = &resolved
    else {
        panic!("expected `StopReason`, got {resolved:?}");
    };
    assert_eq!(*id, global_id(&checked, "StopReason"));
}

#[test]
fn receive_with_inconsistent_arm_tails_diagnoses() {
    // Business arm yields `StopReason`; lifecycle arm yields `Int`.
    // The join must fail with the same message shape `match` uses.
    let source = "
        fn main -> StopReason
          receive
            pair: Pair<Int, Option<ReplyTo<Int>>> -> StopReason.Normal
            event: Lifecycle -> 0
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("inconsistent types")),
        "expected arm-join diagnostic, got {messages:?}",
    );
}

#[test]
fn ref_call_accepts_union_member_arg() {
    // `impl Process<_, MsgA | MsgB, _>` makes `spawn`'s return
    // `Ref<MsgA | MsgB, _>`. Calling `.call(MsgA.Ping(...))` must
    // accept — the receiver's slot already pre-binds `M → MsgA |
    // MsgB`, and the arg-driven unification of `M → MsgA` is a
    // union-member compatibility, not a conflict.
    let source = "
        enum MsgA
          Ping(String)
        end

        enum MsgB
          Pong(Int)
        end

        struct ParentConfig
        end

        struct Parent
          count: Int
        end

        impl Process<ParentConfig, MsgA | MsgB, String> for Parent
          fn start(move config: ParentConfig) -> Result<Self, StopReason>
            Result.Ok(Parent{count: 0})
          end

          fn handle(move self, msg: MsgA | MsgB, from: Option<ReplyTo<String>>) -> Step<Self>
            Step.Continue(self)
          end
        end

        fn main
          p = spawn Parent.start(ParentConfig{})
          p.call(MsgA.Ping(\"hello\"), 5000)
          p.call(MsgB.Pong(42), 5000)
        end
        ";
    // `typecheck` panics on any diagnostic; reaching this point
    // means the union-member acceptance fired and the program
    // checked cleanly.
    let _checked = typecheck(&dedent(source));
}

#[test]
fn ref_call_rejects_non_member_arg() {
    // Negative pin: arg outside the declared union still produces
    // the operand-conflict diagnostic. The union-member relaxation
    // is one-direction-only — random outsider types are not OK.
    let source = "
        enum MsgA
          Ping(String)
        end

        enum MsgB
          Pong(Int)
        end

        enum MsgC
          Other
        end

        struct ParentConfig
        end

        struct Parent
          count: Int
        end

        impl Process<ParentConfig, MsgA | MsgB, String> for Parent
          fn start(move config: ParentConfig) -> Result<Self, StopReason>
            Result.Ok(Parent{count: 0})
          end

          fn handle(move self, msg: MsgA | MsgB, from: Option<ReplyTo<String>>) -> Step<Self>
            Step.Continue(self)
          end
        end

        fn main
          p = spawn Parent.start(ParentConfig{})
          p.call(MsgC.Other, 5000)
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("cannot be both") && m.contains("MsgC")),
        "expected operand-conflict diagnostic mentioning `MsgC`, got {messages:?}",
    );
}
