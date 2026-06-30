//! Tree-walking interpreter over a sealed [`IRProgram`] / [`IRScript`].
//! Parameterized over a [`CallResolver`] so both IR shapes share the
//! per-instruction execution, frame management, and terminator
//! dispatch code; only callee lookup differs. Operator math lives in
//! [`crate::ops`].

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::{Duration, Instant};

use koja_ir::{
    BinaryEndian, BinarySign, BranchTarget, ConcatKind, ConstValue, EnumPayloadInit, FunctionKind,
    IRBasicBlock, IRBlockId, IRConstantValue, IREnumDecl, IRFunction, IRInstruction, IRLocalId,
    IRProgram, IRScript, IRStructDecl, IRSymbol, IRTerminator, IRType, IRVariantPayload,
    IRVariantTag, LoweredBinaryMatchLayout, LoweredBinaryPattern, LoweredBinarySegment,
    ReceiveAfter, ReceiveArm, ReceiveTag, ResolvedBinaryLayout, ValueId,
};
use koja_runtime_core::{CrashInfo, Driver, Readiness, Tag};

use crate::error::RuntimeError;
use crate::externs;
use crate::intrinsics;
use crate::ops::{apply_binary_op, apply_unary_op};
use crate::reactor::EvalReactor;
use crate::scheduler::{
    self, CoreHandle, EvalClock, EvalDriver, EvalExecutor, EvalMessage, EvalSignals, EvalTable,
    ProcessFuture, YieldOnce, block_on,
};
use crate::value::{EnumPayload, Value};

/// A boxed, lifetime-bound interpreter future. Every function on the
/// suspension-reachable call tree returns one so the tree can `.await`
/// a process park (`receive` / `io_block`) and so the mutual recursion
/// type-checks — a boxed `dyn Future` breaks the otherwise-infinite
/// `impl Future` cycle between the call-tree functions.
type EvalFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, RuntimeError>> + 'a>>;

pub struct Interpreter;

impl Interpreter {
    /// Execute the project-mode entry and return its result. The
    /// entry is always a [`FunctionKind::ProcessEntryWrapper`] (seal
    /// guarantees it); the interpreter executes the IR-synthesized
    /// `<state>.__entry_body` the wrapper's IR `Call` names — the
    /// full `start` → `run` → `StopReason.code` dispatch lives there
    /// — and reports the resulting exit code as a [`Value::Int`].
    /// `args` carries the user-facing program arguments (everything
    /// after the program name): a `Process<List<String>, _, _>`
    /// entry receives them as its config, mirroring the LLVM
    /// trampoline's `koja_rt_build_argv` (which skips `argv[0]`);
    /// other config types zero-init via [`default_value_for_type`].
    pub fn run_program(program: &IRProgram, args: &[String]) -> Result<Value, RuntimeError> {
        let entry = program.entry_function();
        assert!(
            matches!(entry.kind, FunctionKind::ProcessEntryWrapper { .. }),
            "interpreter: program entry `{}` is not a `ProcessEntryWrapper` (seal violation)",
            entry.symbol,
        );
        let args = args.to_vec();

        // Boot PID 1 (the entry process) into a fresh cooperative core, then
        // hand the run loop to the shared `CooperativeDriver`. The entry's
        // `StopReason`-derived exit code surfaces through `exit_cell` — the
        // process future has `Output = ()`, so it stashes its `Value` result
        // here for `run_program` to return once the driver tears down.
        let core: CoreHandle = Rc::new(RefCell::new(EvalTable::new()));
        let _guard = scheduler::install_runtime(Rc::clone(&core));
        let main = core.borrow_mut().spawn(());

        let exit_cell: Rc<RefCell<Option<Result<Value, RuntimeError>>>> =
            Rc::new(RefCell::new(None));
        let entry_future: ProcessFuture = {
            let cell = Rc::clone(&exit_cell);
            Box::pin(async move {
                let result = run_entry_body(program, entry, &args).await;
                *cell.borrow_mut() = Some(result);
            })
        };

        let executor = EvalExecutor::new(Rc::clone(&core), program);
        executor.install_future(main, entry_future);

        // The driver installs the signal handlers (SIGTERM/SIGINT/SIGHUP)
        // and drains them into PID 1's mailbox, the same boot the native
        // runtime performs. Draining is gated on the program actually
        // having a `Lifecycle`-arm `receive`: the latch flags are
        // process-global, so an indifferent run must not steal them from a
        // concurrent one (eval runs share one host process; native ones
        // don't).
        let signals = EvalSignals::new(program_uses_lifecycle(program));
        EvalDriver::new(
            core,
            executor,
            EvalReactor,
            EvalClock,
            signals,
            scheduler::grace_period(),
        )
        .run();

        exit_cell
            .borrow_mut()
            .take()
            .expect("entry process produced no result before shutdown")
    }

    /// Execute a named function from `program` with no arguments and
    /// return its value. Test-facing seam: integration tests lower a
    /// fixture with a synthetic Process entry, then call a fixture
    /// function (e.g. `TestApp.main`) directly and assert on its
    /// runtime [`Value`].
    pub fn run_function(program: &IRProgram, mangled: &str) -> Result<Value, RuntimeError> {
        let function = program
            .function(mangled)
            .unwrap_or_else(|| panic!("interpreter: function `{mangled}` not found in IRProgram"));
        block_on(execute_function(function, Vec::new(), program))
    }

    /// Execute the script-mode implicit body and return its trailing
    /// value. Borrows `script` so the caller can re-run or inspect it
    /// without re-lowering.
    ///
    /// Coerces the trailing value to [`Value::Unit`] when the
    /// script's static [`IRScript::return_type`] is `Unit`. See
    /// [`coerce_return`] for the rationale — same shape mirrors
    /// LLVM's `void`-return coercion in
    /// `koja_ir_llvm::emit::emit_terminator`.
    pub fn run_script(script: &IRScript) -> Result<Value, RuntimeError> {
        // Run the implicit body as PID 1 under the shared cooperative
        // driver — same boot as `run_program` — so top-level `spawn` /
        // `receive` / timers / I/O engage the runtime instead of tripping
        // the "runtime not installed" guard. The body's trailing value
        // surfaces through `exit_cell` once the driver tears down.
        let core: CoreHandle = Rc::new(RefCell::new(EvalTable::new()));
        let _guard = scheduler::install_runtime(Rc::clone(&core));
        let main = core.borrow_mut().spawn(());

        let exit_cell: Rc<RefCell<Option<Result<Value, RuntimeError>>>> =
            Rc::new(RefCell::new(None));
        let body_future: ProcessFuture = {
            let cell = Rc::clone(&exit_cell);
            Box::pin(async move {
                *cell.borrow_mut() = Some(run_script_body(script).await);
            })
        };

        let executor = EvalExecutor::new(Rc::clone(&core), script);
        executor.install_future(main, body_future);

        let signals = EvalSignals::new(script_uses_lifecycle(script));
        EvalDriver::new(
            core,
            executor,
            EvalReactor,
            EvalClock,
            signals,
            scheduler::grace_period(),
        )
        .run();

        exit_cell
            .borrow_mut()
            .take()
            .expect("script body produced no result before shutdown")
    }
}

/// Per-call execution frame. SSA values and local-slot storage live
/// in separate maps so slot identity never collides with SSA
/// identity even though both keys happen to be `u32`. `captures`
/// holds the closure environment array (empty for non-closure
/// frames); `LoadCapture` indexes into it directly.
struct Frame {
    captures: Vec<Value>,
    values: BTreeMap<ValueId, Value>,
    locals: BTreeMap<IRLocalId, Value>,
}

impl Frame {
    fn new() -> Self {
        Self::with_captures(Vec::new())
    }

    fn with_captures(captures: Vec<Value>) -> Self {
        Self {
            captures,
            values: BTreeMap::new(),
            locals: BTreeMap::new(),
        }
    }
}

/// Lookup seam used by the per-instruction walker. Both
/// [`IRProgram`] and [`IRScript`] implement this so the same body
/// driver runs over either IR shape; only the underlying maps
/// differ. Function-call resolution + enum-decl lookup share the
/// same trait so each `EnumConstruct` arm has a registry-equivalent
/// handle for materializing the variant's `name` and (for struct
/// payloads) per-field names.
pub(crate) trait CallResolver {
    fn resolve(&self, mangled: &str) -> Option<&IRFunction>;
    fn enum_decl(&self, mangled: &str) -> Option<&IREnumDecl>;
    fn struct_decl(&self, mangled: &str) -> Option<&IRStructDecl>;
    fn constant_value(&self, mangled: &str) -> Option<&IRConstantValue>;
}

impl CallResolver for IRProgram {
    fn resolve(&self, mangled: &str) -> Option<&IRFunction> {
        self.function(mangled)
    }

    fn enum_decl(&self, mangled: &str) -> Option<&IREnumDecl> {
        IRProgram::enum_decl(self, mangled)
    }

    fn struct_decl(&self, mangled: &str) -> Option<&IRStructDecl> {
        IRProgram::struct_decl(self, mangled)
    }

    fn constant_value(&self, mangled: &str) -> Option<&IRConstantValue> {
        IRProgram::constant_value(self, mangled)
    }
}

impl CallResolver for IRScript {
    fn resolve(&self, mangled: &str) -> Option<&IRFunction> {
        self.function(mangled)
    }

    fn enum_decl(&self, mangled: &str) -> Option<&IREnumDecl> {
        IRScript::enum_decl(self, mangled)
    }

    fn struct_decl(&self, mangled: &str) -> Option<&IRStructDecl> {
        IRScript::struct_decl(self, mangled)
    }

    fn constant_value(&self, mangled: &str) -> Option<&IRConstantValue> {
        IRScript::constant_value(self, mangled)
    }
}

/// Outcome of one pass through a function body. `Done` carries the
/// `Return`'s value; `TailRestart` carries the new positional args
/// for the surrounding [`execute_function`] trampoline to rebind
/// before re-walking the body. Surfacing tail restarts as a
/// distinct [`Result::Ok`] payload (rather than a special
/// [`RuntimeError`]) keeps the control-flow signal off the error
/// channel and out of any `?` propagation site.
enum BlockOutcome {
    Done(Value),
    TailRestart(Vec<Value>),
}

/// Run a [`FunctionKind::ProcessEntryWrapper`] entry's body as PID 1.
/// The wrapper itself is a backend ABI shim; the full `start` → `run` →
/// `StopReason.code` dispatch lives in the IR-synthesized
/// `<state>.__entry_body` its IR `Call` names, which the interpreter
/// executes directly with the argv-derived (or default) config. The
/// returned [`Value::Int`] is the exit code — analogous to what the LLVM
/// shim stores into `__koja_exit_code`. Driven by the
/// [`crate::scheduler::EvalExecutor`] (not [`block_on`]) so a `receive`
/// inside it parks against the core mailbox.
async fn run_entry_body<'a>(
    program: &'a IRProgram,
    entry: &'a IRFunction,
    args: &[String],
) -> Result<Value, RuntimeError> {
    let config_type =
        entry
            .params
            .first()
            .map(|p| &p.ty)
            .ok_or_else(|| RuntimeError::Unsupported {
                detail: format!(
                    "process entry wrapper `{}` has no config parameter",
                    entry.symbol,
                ),
            })?;
    let config_value = if is_argv_shaped(config_type) {
        argv_value(args)
    } else {
        default_value_for_type(config_type, program)?
    };
    let body_fn =
        process_body_of(program, &entry.symbol).ok_or_else(|| RuntimeError::Unsupported {
            detail: format!(
                "process entry wrapper `{}` IR body carries no resolvable process-body call",
                entry.symbol,
            ),
        })?;
    execute_function(body_fn, vec![config_value], program).await
}

/// Run the script-mode implicit body as PID 1: walk its blocks to the
/// trailing value, coercing to [`Value::Unit`] when the script's static
/// return type is `Unit`. The async analogue of the former synchronous
/// `run_script`, so a top-level `receive` parks against the core mailbox.
async fn run_script_body(script: &IRScript) -> Result<Value, RuntimeError> {
    let mut frame = Frame::new();
    match execute_blocks(&script.blocks, &mut frame, script).await? {
        BlockOutcome::Done(value) => Ok(coerce_return(value, &script.return_type)),
        BlockOutcome::TailRestart(_) => panic!(
            "interpreter: script body produced a `TailCall` terminator — \
             tail-call rewrite never targets the implicit script body",
        ),
    }
}

/// Whether any of `blocks` has a `receive` with a `Lifecycle` arm — i.e.
/// whether the run observes OS lifecycle signals at all. Gates signal
/// draining so a run that ignores lifecycle events leaves the
/// process-global signal latches for a run that wants them.
fn blocks_use_lifecycle<'a>(blocks: impl Iterator<Item = &'a IRBasicBlock>) -> bool {
    blocks
        .flat_map(|block| &block.instructions)
        .any(|instruction| {
            matches!(
                instruction,
                IRInstruction::Receive { arms, .. }
                    if arms.iter().any(|arm| arm.tag == ReceiveTag::Lifecycle)
            )
        })
}

/// [`blocks_use_lifecycle`] over every function body in `program`.
fn program_uses_lifecycle(program: &IRProgram) -> bool {
    blocks_use_lifecycle(
        program
            .packages
            .iter()
            .flat_map(|package| package.functions.values())
            .flat_map(|function| &function.blocks),
    )
}

/// [`blocks_use_lifecycle`] over the script's helper functions and its
/// implicit top-level body.
fn script_uses_lifecycle(script: &IRScript) -> bool {
    let function_blocks = script
        .packages
        .iter()
        .flat_map(|package| package.functions.values())
        .flat_map(|function| &function.blocks);
    blocks_use_lifecycle(function_blocks.chain(script.blocks.iter()))
}

/// Resolve a process wrapper's body — the [`FunctionKind::Regular`]
/// function its single IR `Call` names. Shared by the entry boot and the
/// `spawn` path: a `ProcessEntryWrapper` / `SpawnWrapper` is a pure ABI
/// shim whose body holds the real `start` → `run` dispatch. `None` only
/// for a malformed wrapper (seal violation); callers decide whether that
/// is an error or a panic.
fn process_body_of<'a, R: CallResolver>(
    resolver: &'a R,
    wrapper: &IRSymbol,
) -> Option<&'a IRFunction> {
    let wrapper_fn = resolver.resolve(wrapper.mangled())?;
    let body_symbol = wrapper_fn
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            IRInstruction::Call { callee, .. } => Some(callee),
            _ => None,
        })?;
    resolver.resolve(body_symbol.mangled())
}

/// Build the boxed process future a `spawn` site installs: run the spawn
/// wrapper's body with `config`, discarding its `Unit` result (the
/// scheduler owns the spawned process's lifecycle). A missing body is a
/// seal violation, since `Spawn::wrapper` always names a `SpawnWrapper`.
pub(crate) fn build_spawn_future<'a, R: CallResolver>(
    resolver: &'a R,
    wrapper: &IRSymbol,
    config: Value,
) -> ProcessFuture<'a> {
    let body_fn = process_body_of(resolver, wrapper).unwrap_or_else(|| {
        panic!(
            "interpreter: spawn wrapper `{wrapper}` has no process body — seal invariant violation"
        )
    });
    Box::pin(async move {
        if let Err(error) = execute_function(body_fn, vec![config], resolver).await {
            scheduler::record_crash(render_process_crash(&error));
        }
    })
}

/// Render a crashed process body's diagnostic and capture its
/// [`CrashInfo`]. The eval analog of native's `render_diagnostic`: a
/// user `Kernel.panic` is a [`RuntimeError::Panicked`] whose message is
/// already the panic text; any other escaping error is reported by its
/// `Display`. Eval has no host backtrace to walk, so `backtrace` is empty.
fn render_process_crash(error: &RuntimeError) -> CrashInfo {
    let message = match error {
        RuntimeError::Panicked { message } => message.clone(),
        other => other.to_string(),
    };
    eprintln!("** (panic) {message}");
    CrashInfo {
        backtrace: String::new(),
        message,
    }
}

/// Whether a process-entry config type is `List<String>` — the one
/// shape that receives host argv instead of a zero-init default.
/// Mirrors the LLVM trampoline's `argv_shaped` test in
/// `koja_ir_llvm::main_wrapper::emit_process_entry_main`.
fn is_argv_shaped(config_type: &IRType) -> bool {
    matches!(
        config_type,
        IRType::List(element) if matches!(**element, IRType::String)
    )
}

/// Materialize program arguments as the `List<String>` config value
/// for an argv-shaped entry. `args` already excludes the program
/// name, matching `koja_rt_build_argv`'s `argv[0]` skip.
fn argv_value(args: &[String]) -> Value {
    let strings = args
        .iter()
        .map(|arg| Value::string(arg.as_bytes()))
        .collect();
    Value::List(std::rc::Rc::new(std::cell::RefCell::new(strings)))
}

/// Build a fresh interpreter [`Value`] suitable as the entry's config
/// argument. Mirrors the LLVM trampoline's zero-init shape: empty
/// structs round-trip as `Value::Struct` with no fields, `List<T>`
/// produces an empty list, and primitive scalars default to their
/// zero element. The argv-shaped `List<String>` config never reaches
/// this helper — [`run_entry_body`] routes it through [`argv_value`]
/// first.
fn default_value_for_type(ty: &IRType, program: &IRProgram) -> Result<Value, RuntimeError> {
    match ty {
        IRType::Bool => Ok(Value::Bool(false)),
        IRType::Float32 => Ok(Value::Float32(0.0)),
        IRType::Float64 => Ok(Value::Float64(0.0)),
        IRType::Int8
        | IRType::Int16
        | IRType::Int32
        | IRType::Int64
        | IRType::UInt8
        | IRType::UInt16
        | IRType::UInt32
        | IRType::UInt64 => Ok(Value::Int(0)),
        IRType::List(_) => Ok(Value::List(std::rc::Rc::new(std::cell::RefCell::new(
            Vec::new(),
        )))),
        IRType::String => Ok(Value::string(Vec::new())),
        IRType::Struct(symbol) => {
            let decl =
                program
                    .struct_decl(symbol.mangled())
                    .ok_or_else(|| RuntimeError::Unsupported {
                        detail: format!(
                            "interpreter: cannot build default value for unknown struct `{symbol}`",
                        ),
                    })?;
            let mut fields = Vec::with_capacity(decl.fields.len());
            for field in &decl.fields {
                fields.push(default_value_for_type(&field.ir_type, program)?);
            }
            Ok(Value::Struct {
                symbol: symbol.clone(),
                fields,
            })
        }
        IRType::Unit => Ok(Value::Unit),
        other => Err(RuntimeError::Unsupported {
            detail: format!(
                "interpreter: cannot synthesize a default value for process-entry config type \
                 `{other:?}`",
            ),
        }),
    }
}

/// Coerce a body-returned [`Value`] to [`Value::Unit`] when the
/// function (or script body) declares [`IRType::Unit`] as its
/// return type.
///
/// IR lowering threads the trailing expression's SSA value through
/// `Return { Some(id) }` even for void-returning functions — the
/// IR comment in `koja_ir::lower::body::finalize_open_flow`
/// notes the value is tracked for seal / dominator analysis but
/// is unobservable at the type level. The LLVM backend collapses
/// this to `ret void` in `koja_ir_llvm::emit::emit_terminator`;
/// without this coercion the interpreter would propagate the
/// trailing temp (e.g. `STDOUT.write`'s `Result<Int64, String>`
/// inside `IO.puts`) and callers would see a richer-than-declared
/// runtime shape. Centralizing the coercion at every body exit
/// keeps the two backends aligned.
fn coerce_return(value: Value, return_type: &IRType) -> Value {
    if matches!(return_type, IRType::Unit) {
        Value::Unit
    } else {
        value
    }
}

/// Run `function` in a fresh frame with `args` positionally bound to
/// its param `ValueId`s. Param promotion (entry-block `LocalDecl` +
/// `LocalWrite`) means the body reads from the slot, not the raw
/// param id; seeding `frame.values` keeps the promotion's
/// `LocalWrite { value: param.id }` resolvable. `@intrinsic`-tagged
/// functions route to [`crate::intrinsics`].
///
/// Wraps the body walk in a tail-call trampoline: an
/// [`IRTerminator::TailCall`] surfaces from `execute_blocks` as
/// `BlockOutcome::TailRestart(new_args)`, which we re-seed the
/// frame with and re-enter the same body, keeping host-stack
/// usage flat across any number of recursive tail calls.
fn execute_function<'a, R: CallResolver>(
    function: &'a IRFunction,
    args: Vec<Value>,
    resolver: &'a R,
) -> EvalFuture<'a, Value> {
    Box::pin(async move {
        let mut args = args;
        debug_assert_eq!(
            function.params.len(),
            args.len(),
            "arity mismatch calling `{}`: {} params vs {} args (typecheck invariant)",
            function.symbol,
            function.params.len(),
            args.len(),
        );
        match &function.kind {
            FunctionKind::Intrinsic(id) => {
                return intrinsics::dispatch(id, function, &args, resolver).await;
            }
            FunctionKind::Extern(attrs) => {
                let c_symbol = attrs
                    .link_name
                    .as_deref()
                    .unwrap_or_else(|| function.symbol.last_segment());
                return match externs::dispatch(c_symbol, &args).await {
                    Some(result) => result,
                    None => Err(RuntimeError::ExternNotSupported {
                        symbol: function.symbol.mangled().to_string(),
                    }),
                };
            }
            // Acquisition / release glue is a no-op under the interpreter:
            // every host `Value` is independent (deep-cloned on `lookup`)
            // and reclaimed by the host GC, so a clone is a rebind of the
            // argument and a drop returns unit. Short-circuiting here means
            // eval never executes a glue body — neither the aggregate CFG
            // `elaborate` synthesizes nor the empty collection shell the
            // LLVM backend fills.
            FunctionKind::CloneGlue | FunctionKind::DeepCopyGlue => {
                return Ok(args.into_iter().next().unwrap_or(Value::Unit));
            }
            FunctionKind::DropGlue => return Ok(Value::Unit),
            FunctionKind::Closure { .. } => panic!(
                "interpreter: direct `Call` to closure body `{}` — must dispatch via \
             `CallClosure` (seal invariant violation)",
                function.symbol,
            ),
            // The env glue siblings exist only to back the LLVM env block
            // ABI (teardown via the header's `drop_fn`, process-boundary
            // copy via `copy_fn`). The interpreter's `Value::Closure`
            // carries its captures by value and is reclaimed by the host
            // GC, so it never calls (or even references) either.
            FunctionKind::CopyClosureGlue { .. } => panic!(
                "interpreter: `$copy_env$` env deep-copy glue `{}` is LLVM-only — eval copies \
             closures structurally and never invokes it",
                function.symbol,
            ),
            FunctionKind::DropClosureGlue { .. } => panic!(
                "interpreter: `$drop_env$` capture-release glue `{}` is LLVM-only — eval reclaims \
             closures via the host GC and never invokes it",
                function.symbol,
            ),
            FunctionKind::SpawnWrapper { .. } => {
                return Err(RuntimeError::Unsupported {
                    detail: format!(
                        "spawn wrapper `{}` cannot be invoked directly under the interpreter; \
                     spawn/receive scheduling lives in the LLVM runtime",
                        function.symbol,
                    ),
                });
            }
            FunctionKind::ProcessEntryWrapper { .. } => {
                return Err(RuntimeError::Unsupported {
                    detail: format!(
                        "process entry wrapper `{}` cannot be invoked directly; use \
                     `Interpreter::run_program`, which dispatches through state.start / \
                     state.run for ProcessEntryWrapper entries",
                        function.symbol,
                    ),
                });
            }
            FunctionKind::Regular => {}
        }
        loop {
            let mut frame = Frame::new();
            for (param, value) in function.params.iter().zip(args.into_iter()) {
                frame.values.insert(param.id, value);
            }
            match execute_blocks(&function.blocks, &mut frame, resolver).await? {
                BlockOutcome::Done(value) => {
                    return Ok(coerce_return(value, &function.return_type));
                }
                BlockOutcome::TailRestart(new_args) => {
                    args = new_args;
                }
            }
        }
    })
}

/// Dispatch a [`FunctionKind::Closure`] body with its captured
/// environment. Mirrors [`execute_function`] for `Regular` bodies,
/// but seeds `frame.captures` so [`IRInstruction::LoadCapture`] can
/// index into the env array. `captures.len()` matches the body's
/// `env_layout` (seal invariant).
fn execute_closure_function<'a, R: CallResolver>(
    function: &'a IRFunction,
    args: Vec<Value>,
    captures: Vec<Value>,
    resolver: &'a R,
) -> EvalFuture<'a, Value> {
    Box::pin(async move {
        debug_assert_eq!(
            function.params.len(),
            args.len(),
            "arity mismatch calling closure body `{}`: {} params vs {} args",
            function.symbol,
            function.params.len(),
            args.len(),
        );
        let env_layout = match &function.kind {
            FunctionKind::Closure { env_layout } => env_layout,
            other => panic!(
                "interpreter: `execute_closure_function` on non-Closure kind {other:?} for `{}`",
                function.symbol,
            ),
        };
        debug_assert_eq!(
            env_layout.len(),
            captures.len(),
            "env arity mismatch calling closure body `{}`: layout has {} entries vs {} captures",
            function.symbol,
            env_layout.len(),
            captures.len(),
        );
        let mut frame = Frame::with_captures(captures);
        for (param, value) in function.params.iter().zip(args.into_iter()) {
            frame.values.insert(param.id, value);
        }
        match execute_blocks(&function.blocks, &mut frame, resolver).await? {
            BlockOutcome::Done(value) => Ok(coerce_return(value, &function.return_type)),
            BlockOutcome::TailRestart(_) => panic!(
                "interpreter: closure body `{}` produced a `TailCall` terminator — \
             tail-call rewrite is not enabled for closures yet",
                function.symbol,
            ),
        }
    })
}

/// Drive a function body starting at `blocks[0]` until a `Return`
/// exits. The frame is shared across every block; unknown branch
/// targets panic per the seal contract. Loop back-edges fall out of
/// [`IRTerminator::Branch`] to any [`IRBlockId`] — the dispatcher
/// treats them like any other branch. The interpreter imposes no
/// step / iteration cap: real programs have legitimate infinite
/// loops (a server's main loop, an actor's `receive`, the eventual
/// `loop { ... }` construct). Test harnesses provide their own
/// timeouts at the binary level if a test accidentally diverges.
fn execute_blocks<'a, R: CallResolver>(
    blocks: &'a [IRBasicBlock],
    frame: &'a mut Frame,
    resolver: &'a R,
) -> EvalFuture<'a, BlockOutcome> {
    Box::pin(async move {
        let mut current = blocks
            .first()
            .expect("sealed function has at least one basic block")
            .id;
        'blocks: loop {
            let block = find_block(blocks, current);
            for instruction in &block.instructions {
                // `Receive` transfers control to an arm (or after) body
                // block instead of defining a value — lowering places it
                // last in its block with an `Unreachable` terminator —
                // so it dispatches here rather than in
                // `execute_instruction`.
                if let IRInstruction::Receive { after, arms, .. } = instruction {
                    current = execute_receive(arms, after.as_ref(), frame, resolver).await?;
                    continue 'blocks;
                }
                execute_instruction(instruction, frame, resolver).await?;
            }
            match &block.terminator {
                IRTerminator::Branch(target) => {
                    bind_block_params(target, blocks, &mut frame.values)?;
                    current = target.block;
                }
                IRTerminator::CondBranch {
                    cond,
                    else_target,
                    then_target,
                } => {
                    let cond_value = lookup(&frame.values, *cond)?;
                    let Value::Bool(b) = cond_value else {
                        return Err(RuntimeError::TypeMismatch {
                            detail: format!(
                                "cond_branch expects a Bool condition; got {cond_value}",
                            ),
                        });
                    };
                    let chosen = if b { then_target } else { else_target };
                    bind_block_params(chosen, blocks, &mut frame.values)?;
                    current = chosen.block;
                }
                IRTerminator::Return { value: None } => return Ok(BlockOutcome::Done(Value::Unit)),
                IRTerminator::Return { value: Some(id) } => {
                    return lookup(&frame.values, *id).map(BlockOutcome::Done);
                }
                IRTerminator::TailCall { args, .. } => {
                    let mut arg_values = Vec::with_capacity(args.len());
                    for arg in args {
                        arg_values.push(lookup(&frame.values, *arg)?);
                    }
                    return Ok(BlockOutcome::TailRestart(arg_values));
                }
                IRTerminator::Unreachable => return Err(RuntimeError::UnreachableExecuted),
            }
        }
    })
}

/// Evaluate `target.args` in the predecessor's value-map and bind
/// the resulting [`Value`]s to the target block's
/// [`koja_ir::BlockParam::dest`] ids before stepping into the
/// target. Block params are SSA defs available on entry to the
/// block; backends bind them on edge traversal so the body's
/// instructions see them as ordinary `ValueId`s.
///
/// Seal asserts arg/param arity match, so a length mismatch is a
/// compiler bug; we panic with the same shape as `find_block`'s
/// missing-block panic. Args are looked up before bindings are
/// inserted so a hypothetical self-loop's arg list reads the
/// pre-edge values, not the new param bindings.
fn bind_block_params(
    target: &BranchTarget,
    blocks: &[IRBasicBlock],
    values: &mut BTreeMap<ValueId, Value>,
) -> Result<(), RuntimeError> {
    let target_block = find_block(blocks, target.block);
    if target.args.len() != target_block.params.len() {
        panic!(
            "interpreter: branch to `{}` passes {} arg(s) but target declares {} param(s) — \
             seal invariant violation",
            target.block,
            target.args.len(),
            target_block.params.len(),
        );
    }
    let bindings: Vec<(ValueId, Value)> = target
        .args
        .iter()
        .zip(target_block.params.iter())
        .map(|(arg, param)| Ok((param.dest, lookup(values, *arg)?)))
        .collect::<Result<_, RuntimeError>>()?;
    for (param_id, value) in bindings {
        values.insert(param_id, value);
    }
    Ok(())
}

/// Execute an [`IRInstruction::Receive`], returning the basic block
/// control transfers to.
///
/// Parks against the running process's core mailbox: pop a delivered
/// message (system traffic before business), dispatch it to a matching
/// arm, else (when an `after` clause is present) check the deadline, else
/// park `Blocked` and yield back to the driver. The driver re-resumes
/// only once a delivery or the deadline promotes the process, so a
/// delivered message beats an already-expired timeout — the runtime's
/// message-before-timeout priority. A `receive` with no matching delivery
/// and no `after` parks indefinitely, exactly as the native runtime does
/// (a genuine deadlock is a program bug). A synthesized
/// `ReceiveTag::IOReady` arm is inert until the reactor phase.
fn execute_receive<'a, R: CallResolver>(
    arms: &'a [ReceiveArm],
    after: Option<&'a ReceiveAfter>,
    frame: &'a mut Frame,
    resolver: &'a R,
) -> EvalFuture<'a, IRBlockId> {
    Box::pin(async move {
        let deadline = after
            .map(|clause| {
                let value = lookup(&frame.values, clause.timeout)?;
                let Value::Int(ms) = value else {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!("receive `after` expects an Int timeout; got {value}"),
                    });
                };
                Ok(Instant::now() + Duration::from_millis(ms.max(0) as u64))
            })
            .transpose()?;

        let pid = scheduler::current_pid();
        loop {
            if let Some(message) = scheduler::pop_received(pid)
                && let Some(block) = dispatch_received(message, arms, frame, resolver)
            {
                return Ok(block);
            }
            if let Some(deadline) = deadline
                && Instant::now() >= deadline
            {
                let clause = after.expect("deadline implies an after clause");
                return Ok(clause.body);
            }
            scheduler::park_receive(pid, deadline);
            YieldOnce::new().await;
        }
    })
}

/// Dispatch a mailbox message popped during `receive` to the arm whose
/// tag matches, binding the arm's payload local and returning its body
/// block. `None` when no arm matches (the message is dropped). Business
/// traffic binds a `Pair<M, Option<ReplyTo<R>>>` (built from the arm's
/// payload type); lifecycle traffic binds the `Lifecycle` enum value.
fn dispatch_received<R: CallResolver>(
    message: EvalMessage,
    arms: &[ReceiveArm],
    frame: &mut Frame,
    resolver: &R,
) -> Option<IRBlockId> {
    match message.tag {
        Tag::Business => {
            let arm = arms.iter().find(|arm| arm.tag == ReceiveTag::Business)?;
            let payload = intrinsics::build_business_payload(&arm.payload_type, message, resolver);
            frame.locals.insert(arm.payload_local, payload);
            Some(arm.body)
        }
        // The reactor delivers a fully-built `IOReady` enum value
        // ([`build_io_ready_value`]); bind it into the synthesized
        // `IOReady` arm (whose body reshapes it into the business `Pair`).
        Tag::IOReady => {
            let arm = arms.iter().find(|arm| arm.tag == ReceiveTag::IOReady)?;
            frame.locals.insert(arm.payload_local, message.value);
            Some(arm.body)
        }
        Tag::Lifecycle => {
            let arm = arms.iter().find(|arm| arm.tag == ReceiveTag::Lifecycle)?;
            let Value::Int(variant) = message.value else {
                panic!(
                    "interpreter: lifecycle message carries non-Int variant `{}`",
                    message.value,
                );
            };
            let payload = lifecycle_value(arm, variant, resolver);
            frame.locals.insert(arm.payload_local, payload);
            Some(arm.body)
        }
        // Replies never surface through `pop_received` (they live in the
        // one-shot reply slot, read by `Ref.call`).
        Tag::Reply => None,
    }
}

/// Materialize the `Lifecycle` enum value for a drained signal.
/// `variant` is the variant index in declaration order (Shutdown=0,
/// Interrupt=1, Reload=2) — the same mapping
/// `koja_runtime::signals::drain` documents.
fn lifecycle_value<R: CallResolver>(arm: &ReceiveArm, variant: i64, resolver: &R) -> Value {
    let IRType::Enum(symbol) = &arm.payload_type else {
        panic!(
            "interpreter: lifecycle receive arm payload type is not an enum \
             (got `{:?}`) — seal invariant violation",
            arm.payload_type,
        );
    };
    let decl = resolver.enum_decl(symbol.mangled()).unwrap_or_else(|| {
        panic!("interpreter: enum `{symbol}` missing from IR — seal invariant violation")
    });
    let variant_decl = decl.variants.get(variant as usize).unwrap_or_else(|| {
        panic!(
            "interpreter: lifecycle variant index {variant} out of range for `{symbol}` \
             ({} variant(s) declared)",
            decl.variants.len(),
        )
    });
    Value::Enum {
        name: variant_decl.name.clone(),
        payload: EnumPayload::Unit,
        symbol: symbol.clone(),
        tag: IRVariantTag(variant as u8),
    }
}

/// Mangled symbol of the kernel `IO.Ready` enum (`lib/global/src/io.koja`).
/// Non-generic, so its symbol is the bare package-qualified name — the
/// same constant the `koja-ir` `deliver_io_ready` elaborate pass keys on.
const IO_READY_SYMBOL: &str = "Global.IO.Ready";

/// Materialize the `IOReady.{Read,Write,Error}(Fd)` value the reactor
/// delivers to a `Fd.watch` owner. Built at send time (the driver's
/// readiness pass) so the receiver's synthesized `ReceiveTag::IOReady` arm
/// just binds it. `readiness` selects the variant; `fd` fills the wrapped
/// `Fd{ descriptor }`, whose struct symbol is recovered from the variant
/// payload rather than fabricated.
pub(crate) fn build_io_ready_value<R: CallResolver>(
    resolver: &R,
    readiness: Readiness,
    fd: i32,
) -> Value {
    let variant_name = match readiness {
        Readiness::Error => "Error",
        Readiness::Readable => "Read",
        Readiness::Writable => "Write",
    };
    let decl = resolver.enum_decl(IO_READY_SYMBOL).unwrap_or_else(|| {
        panic!("interpreter: kernel enum `{IO_READY_SYMBOL}` missing from IR — a watched fd fired but `IOReady` is not in the program")
    });
    let variant = decl
        .variants
        .iter()
        .find(|variant| variant.name == variant_name)
        .unwrap_or_else(|| {
            panic!("interpreter: `{IO_READY_SYMBOL}` has no `{variant_name}` variant — seal invariant violation")
        });
    let IRVariantPayload::Tuple(types) = &variant.payload else {
        panic!(
            "interpreter: `IOReady.{variant_name}` payload is not a tuple — seal invariant violation"
        );
    };
    let [IRType::Struct(fd_symbol)] = types.as_slice() else {
        panic!(
            "interpreter: `IOReady.{variant_name}` payload is not a single `Fd` struct — seal invariant violation"
        );
    };
    Value::Enum {
        name: variant_name.to_string(),
        payload: EnumPayload::Tuple(vec![Value::Struct {
            symbol: fd_symbol.clone(),
            fields: vec![Value::Int(i64::from(fd))],
        }]),
        symbol: decl.symbol.clone(),
        tag: variant.tag,
    }
}

fn find_block(blocks: &[IRBasicBlock], id: IRBlockId) -> &IRBasicBlock {
    blocks
        .iter()
        .find(|b| b.id == id)
        .unwrap_or_else(|| panic!("interpreter: block `{id}` missing — seal invariant violation"))
}

fn execute_instruction<'a, R: CallResolver>(
    instruction: &'a IRInstruction,
    frame: &'a mut Frame,
    resolver: &'a R,
) -> EvalFuture<'a, ()> {
    Box::pin(async move {
        match instruction {
            IRInstruction::BinaryConstruct {
                dest,
                layout,
                segments,
            } => {
                let value = construct_binary_literal(*layout, segments, frame)?;
                frame.values.insert(*dest, value);
                Ok(())
            }
            IRInstruction::BinaryOp { dest, lhs, op, rhs } => {
                let lhs_value = lookup(&frame.values, *lhs)?;
                let rhs_value = lookup(&frame.values, *rhs)?;
                let result = apply_binary_op(*op, lhs_value, rhs_value)?;
                frame.values.insert(*dest, result);
                Ok(())
            }
            IRInstruction::Call { dest, callee, args } => {
                let mut arg_values = Vec::with_capacity(args.len());
                for arg in args {
                    arg_values.push(lookup(&frame.values, *arg)?);
                }
                let callee_fn = resolver.resolve(callee.mangled()).unwrap_or_else(|| {
                    panic!(
                        "interpreter: callee `{callee}` missing from IR — \
                     seal invariant violation",
                    )
                });
                let result = execute_function(callee_fn, arg_values, resolver).await?;
                frame.values.insert(*dest, result);
                Ok(())
            }
            // The host `Value` is deep-cloned on every `lookup`, so a
            // `Clone` is just a re-bind: the result is already an
            // independent copy with no shared backing. The LLVM backend
            // does the real allocation; here the GC handles reclamation.
            // `DeepCopy` (the process-boundary copy) gets the same
            // treatment for the same reason — lookup's clone is already
            // physically independent.
            IRInstruction::Clone { dest, source, .. }
            | IRInstruction::DeepCopy { dest, source, .. } => {
                let value = lookup(&frame.values, *source)?;
                frame.values.insert(*dest, value);
                Ok(())
            }
            IRInstruction::Concat {
                dest,
                kind,
                lhs,
                rhs,
            } => {
                let left = lookup(&frame.values, *lhs)?;
                let right = lookup(&frame.values, *rhs)?;
                let result = concat_values(*kind, &left, &right)?;
                frame.values.insert(*dest, result);
                Ok(())
            }
            IRInstruction::Const { dest, value } => {
                frame.values.insert(*dest, materialize_const(value));
                Ok(())
            }
            IRInstruction::LoadConst {
                dest,
                const_id,
                ty: _,
            } => {
                let pooled = resolver.constant_value(const_id.mangled()).unwrap_or_else(|| {
                panic!(
                    "interpreter: LoadConst `{}` missing from pooled constants — seal invariant violation",
                    const_id.mangled(),
                )
            });
                let value = materialize_pooled_constant(pooled, resolver)?;
                frame.values.insert(*dest, value);
                Ok(())
            }
            IRInstruction::EnumConstruct {
                dest,
                payload,
                tag,
                ty,
            } => {
                let value = materialize_enum(ty, *tag, payload, frame, resolver)?;
                frame.values.insert(*dest, value);
                Ok(())
            }
            IRInstruction::EnumPayloadFieldGet {
                dest,
                payload_index,
                tag,
                value,
                ..
            } => {
                let base = lookup(&frame.values, *value)?;
                let Value::Enum {
                    payload,
                    tag: actual_tag,
                    ..
                } = base
                else {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!("EnumPayloadFieldGet expects an Enum receiver; got {base}"),
                    });
                };
                if actual_tag != *tag {
                    panic!(
                        "interpreter: EnumPayloadFieldGet expected tag {tag} but value carries \
                     tag {actual_tag} — match driver should have gated on a tag check first",
                    );
                }
                let field = match payload {
                    EnumPayload::Tuple(values) => values
                        .into_iter()
                        .nth(*payload_index as usize)
                        .unwrap_or_else(|| {
                            panic!(
                                "interpreter: EnumPayloadFieldGet tuple index {payload_index} \
                             out of range — seal invariant violation",
                            )
                        }),
                    EnumPayload::Struct(fields) => fields
                        .into_iter()
                        .nth(*payload_index as usize)
                        .map(|(_, value)| value)
                        .unwrap_or_else(|| {
                            panic!(
                                "interpreter: EnumPayloadFieldGet struct index {payload_index} \
                             out of range — seal invariant violation",
                            )
                        }),
                    EnumPayload::Unit => panic!(
                        "interpreter: EnumPayloadFieldGet on a Unit variant — seal invariant violation",
                    ),
                };
                frame.values.insert(*dest, field);
                Ok(())
            }
            IRInstruction::EnumTagGet { dest, value, .. } => {
                let base = lookup(&frame.values, *value)?;
                let Value::Enum { tag, .. } = base else {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!("EnumTagGet expects an Enum receiver; got {base}"),
                    });
                };
                frame.values.insert(*dest, Value::Int(i64::from(tag.0)));
                Ok(())
            }
            IRInstruction::FieldGet {
                base,
                dest,
                field_index,
                field_type: _,
                struct_symbol: _,
            } => {
                let base_value = lookup(&frame.values, *base)?;
                let Value::Struct { fields, .. } = base_value else {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!("field_get expects a Struct receiver; got {base_value}",),
                    });
                };
                let field = fields
                    .into_iter()
                    .nth(*field_index as usize)
                    .unwrap_or_else(|| {
                        panic!(
                            "interpreter: FieldGet index {field_index} out of range — \
                         seal invariant violation",
                        )
                    });
                frame.values.insert(*dest, field);
                Ok(())
            }
            IRInstruction::FieldSet {
                base,
                dest,
                field_index,
                field_type: _,
                struct_symbol: _,
                value,
            } => {
                let base_value = lookup(&frame.values, *base)?;
                let Value::Struct { mut fields, symbol } = base_value else {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!("field_set expects a Struct receiver; got {base_value}",),
                    });
                };
                let new_field = lookup(&frame.values, *value)?;
                let slot = fields.get_mut(*field_index as usize).unwrap_or_else(|| {
                    panic!(
                        "interpreter: FieldSet index {field_index} out of range — seal invariant \
                     violation",
                    )
                });
                *slot = new_field;
                frame.values.insert(*dest, Value::Struct { fields, symbol });
                Ok(())
            }
            IRInstruction::DropLocal { .. } => Ok(()),
            // Heap reclamation is handled by the host GC; the IR-level
            // value-keyed drop is a no-op for the interpreter (mirrors
            // [`IRInstruction::DropLocal`] above).
            IRInstruction::DropValue { .. } => Ok(()),
            // The LLVM backend zero-initializes the slot at the decl
            // site so scope-exit drop glue can run on never-written
            // slots (e.g. the payload local of a receive arm that did
            // not fire). Mirror with a `Unit` placeholder — eval's drop
            // glue short-circuits, so the placeholder is only ever
            // observed by a glue-feeding `LocalRead`, never by user
            // code (a user-level read-before-write cannot pass
            // typecheck).
            IRInstruction::LocalDecl { local, .. } => {
                frame.locals.insert(*local, Value::Unit);
                Ok(())
            }
            IRInstruction::LocalRead { dest, local, .. } => {
                let value = frame.locals.get(local).cloned().unwrap_or_else(|| {
                    panic!(
                        "interpreter: `LocalRead` of `{local}` before its `LocalDecl` — \
                     seal invariant violation",
                    )
                });
                frame.values.insert(*dest, value);
                Ok(())
            }
            IRInstruction::LocalWrite { local, value } => {
                let resolved = lookup(&frame.values, *value)?;
                frame.locals.insert(*local, resolved);
                Ok(())
            }
            IRInstruction::StructInit { dest, fields, ty } => {
                let mut materialized = Vec::with_capacity(fields.len());
                for field in fields {
                    materialized.push(lookup(&frame.values, field.value)?);
                }
                frame.values.insert(
                    *dest,
                    Value::Struct {
                        symbol: ty.clone(),
                        fields: materialized,
                    },
                );
                Ok(())
            }
            IRInstruction::UnaryOp { dest, op, operand } => {
                let operand_value = lookup(&frame.values, *operand)?;
                let result = apply_unary_op(*op, operand_value)?;
                frame.values.insert(*dest, result);
                Ok(())
            }
            IRInstruction::CallClosure {
                args,
                callee,
                dest,
                result_ty: _,
            } => {
                let callee_value = lookup(&frame.values, *callee)?;
                let Value::Closure { body, captures } = callee_value else {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!(
                            "CallClosure expects a Closure receiver; got {callee_value}"
                        ),
                    });
                };
                let mut arg_values = Vec::with_capacity(args.len());
                for arg in args {
                    arg_values.push(lookup(&frame.values, *arg)?);
                }
                let body_fn = resolver.resolve(body.mangled()).unwrap_or_else(|| {
                    panic!(
                        "interpreter: closure body `{body}` missing from IR — \
                     seal invariant violation",
                    )
                });
                let result =
                    execute_closure_function(body_fn, arg_values, captures, resolver).await?;
                frame.values.insert(*dest, result);
                Ok(())
            }
            IRInstruction::LoadCapture {
                capture_index,
                dest,
                ty: _,
            } => {
                let value = frame
                    .captures
                    .get(*capture_index as usize)
                    .cloned()
                    .unwrap_or_else(|| {
                        panic!(
                            "interpreter: LoadCapture index {capture_index} out of range \
                         (env has {} entries) — seal invariant violation",
                            frame.captures.len(),
                        )
                    });
                frame.values.insert(*dest, value);
                Ok(())
            }
            IRInstruction::MakeClosure {
                body,
                captures,
                dest,
                ty: _,
            } => {
                let mut env = Vec::with_capacity(captures.len());
                for capture in captures {
                    env.push(lookup(&frame.values, *capture)?);
                }
                frame.values.insert(
                    *dest,
                    Value::Closure {
                        body: body.clone(),
                        captures: env,
                    },
                );
                Ok(())
            }
            // Sized integers are already canonical `Value::Int(i64)`
            // (sign/zero-extended at materialization), so the integer
            // widen is a pass-through; only `Float32 -> Float64`
            // changes representation.
            IRInstruction::NumericWiden { dest, value, .. } => {
                let source = lookup(&frame.values, *value)?;
                let widened = match source {
                    Value::Float32(v) => Value::Float64(f64::from(v)),
                    other => other,
                };
                frame.values.insert(*dest, widened);
                Ok(())
            }
            IRInstruction::Spawn {
                config,
                dest,
                ref_type,
                wrapper,
                ..
            } => {
                // Register the child in the core table now (so its PID is
                // stable for the returned `Ref`) and queue the spawn request;
                // the executor builds and installs the child's future after
                // this resume, before the driver can claim it. `Ref<M, R>`
                // lays out as `{ i64 id }` (see `koja-ir-llvm`'s `pid_from_self`).
                let config_value = lookup(&frame.values, *config)?;
                let pid = scheduler::spawn_child(wrapper.clone(), config_value);
                frame.values.insert(
                    *dest,
                    Value::Struct {
                        symbol: ref_type.clone(),
                        fields: vec![Value::Int(pid)],
                    },
                );
                Ok(())
            }
            IRInstruction::ProcessExit { reason } => {
                let reason = lookup(&frame.values, *reason)?;
                let Value::Int(reason) = reason else {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!("ProcessExit expects an Int reason; got {reason}"),
                    });
                };
                scheduler::process_exit(reason);
                Ok(())
            }
            IRInstruction::SetPriority { tag } => {
                let level = lookup(&frame.values, *tag)?;
                let Value::Int(level) = level else {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!("SetPriority expects an Int tag; got {level}"),
                    });
                };
                scheduler::set_priority(level);
                Ok(())
            }
            IRInstruction::YieldCheck => {
                if scheduler::reduce() {
                    YieldOnce::new().await;
                }
                Ok(())
            }
            IRInstruction::Receive { .. } => panic!(
                "interpreter: `Receive` reached `execute_instruction` — `execute_blocks` \
             intercepts it as a control transfer (lowering places it last in its block)",
            ),
            IRInstruction::UnionWrap {
                dest,
                member_index,
                member_type: _,
                ty,
                value,
            } => {
                let payload = lookup(&frame.values, *value)?;
                let IRType::Union { mangled, .. } = ty else {
                    panic!(
                        "interpreter: UnionWrap target IRType is not Union (got `{ty:?}`) — \
                     IR seal invariant violation",
                    );
                };
                frame.values.insert(
                    *dest,
                    Value::Union {
                        payload: Box::new(payload),
                        symbol: mangled.clone(),
                        tag: *member_index,
                    },
                );
                Ok(())
            }
            IRInstruction::UnionTagGet { dest, ty: _, value } => {
                let base = lookup(&frame.values, *value)?;
                let Value::Union { tag, .. } = base else {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!("UnionTagGet expects a Union receiver; got {base}"),
                    });
                };
                frame.values.insert(*dest, Value::Int(i64::from(tag)));
                Ok(())
            }
            IRInstruction::UnionPayloadGet {
                dest,
                member_index,
                member_type: _,
                ty: _,
                value,
            } => {
                let base = lookup(&frame.values, *value)?;
                let Value::Union {
                    payload,
                    tag: actual_tag,
                    ..
                } = base
                else {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!("UnionPayloadGet expects a Union receiver; got {base}"),
                    });
                };
                if actual_tag != *member_index {
                    panic!(
                        "interpreter: UnionPayloadGet expected member-index {member_index} but value \
                     carries tag {actual_tag} — match driver should have gated on a tag check first",
                    );
                }
                frame.values.insert(*dest, *payload);
                Ok(())
            }
            IRInstruction::BinaryMatch {
                dest,
                layout,
                segments,
                subject,
            } => {
                let subject_value = lookup(&frame.values, *subject)?;
                let matched = execute_binary_match(*layout, segments, &subject_value, frame)?;
                frame.values.insert(*dest, Value::Bool(matched));
                Ok(())
            }
        }
    })
}

fn lookup(values: &BTreeMap<ValueId, Value>, id: ValueId) -> Result<Value, RuntimeError> {
    values
        .get(&id)
        .cloned()
        .ok_or(RuntimeError::ValueUndefined { id })
}

fn materialize_pooled_constant<R: CallResolver>(
    cv: &IRConstantValue,
    resolver: &R,
) -> Result<Value, RuntimeError> {
    match cv {
        IRConstantValue::Primitive(inner) => Ok(materialize_const(inner)),
        IRConstantValue::EnumVariant { tag, ty } => {
            let decl = resolver.enum_decl(ty.mangled()).unwrap_or_else(|| {
                panic!(
                    "interpreter: pooled enum `{}` missing from IR — seal invariant violation",
                    ty.mangled(),
                )
            });
            let variant = decl.variants.get(usize::from(tag.0)).unwrap_or_else(|| {
                panic!(
                    "interpreter: pooled EnumVariant `{}` references tag {:?} past {} variants — \
                         seal invariant violation",
                    ty.mangled(),
                    tag,
                    decl.variants.len(),
                )
            });
            Ok(Value::Enum {
                name: variant.name.clone(),
                payload: EnumPayload::Unit,
                symbol: ty.clone(),
                tag: *tag,
            })
        }
        IRConstantValue::Struct { fields, ty } => {
            let mut materialized = Vec::with_capacity(fields.len());
            for f in fields {
                materialized.push(materialize_pooled_constant(f, resolver)?);
            }
            Ok(Value::Struct {
                symbol: ty.clone(),
                fields: materialized,
            })
        }
    }
}

/// Materialize a [`Value::Enum`] from an `EnumConstruct` payload init.
/// Looks up the enum decl through the resolver, fetches the variant
/// at `tag.0` (constant-time index — seal asserts the tag is in
/// range and matches the payload shape), and zips the init values
/// with the variant's declared shape into an [`EnumPayload`].
///
/// Per-shape:
/// - Unit → `EnumPayload::Unit`.
/// - Tuple → materialize each `ValueId` against `frame.values`.
/// - Struct → zip the (canonicalized, declaration-order) inits with
///   the variant's declared `IRStructField`s so each materialized
///   value pairs with its declared field name in the resulting
///   `Vec<(String, Value)>`.
fn materialize_enum<R: CallResolver>(
    symbol: &IRSymbol,
    tag: IRVariantTag,
    payload: &EnumPayloadInit,
    frame: &Frame,
    resolver: &R,
) -> Result<Value, RuntimeError> {
    let decl = resolver.enum_decl(symbol.mangled()).unwrap_or_else(|| {
        panic!(
            "interpreter: enum `{symbol}` missing from IR — \
             seal invariant violation",
        )
    });
    let variant = decl.variants.get(usize::from(tag.0)).unwrap_or_else(|| {
        panic!(
            "interpreter: EnumConstruct on `{symbol}` references tag {tag} but the decl only \
             declares {} variant(s) — seal invariant violation",
            decl.variants.len(),
        )
    });
    let materialized = match (payload, &variant.payload) {
        (EnumPayloadInit::Unit, IRVariantPayload::Unit) => EnumPayload::Unit,
        (EnumPayloadInit::Tuple(ids), IRVariantPayload::Tuple(_)) => {
            let mut values = Vec::with_capacity(ids.len());
            for id in ids {
                values.push(lookup(&frame.values, *id)?);
            }
            EnumPayload::Tuple(values)
        }
        (EnumPayloadInit::Struct(inits), IRVariantPayload::Struct(declared)) => {
            let mut fields = Vec::with_capacity(inits.len());
            for (init, decl_field) in inits.iter().zip(declared.iter()) {
                let value = lookup(&frame.values, init.value)?;
                fields.push((decl_field.name.clone(), value));
            }
            EnumPayload::Struct(fields)
        }
        (init, declared) => panic!(
            "interpreter: EnumConstruct payload shape mismatch on `{symbol}.{}` \
             (declared {declared:?}, supplied {init:?}) — seal invariant violation",
            variant.name,
        ),
    };
    Ok(Value::Enum {
        name: variant.name.clone(),
        payload: materialized,
        symbol: symbol.clone(),
        tag,
    })
}

/// Apply `<>` to two heap-payload values. Mirrors the LLVM
/// backend's split: `String` / `Binary` are byte-aligned `memcpy`s,
/// `Bits` does sub-byte alignment in Rust (the runtime helper's
/// algorithm). Mismatched [`Value`] kinds vs `kind` surface a
/// `TypeMismatch` — defensive, since seal + typecheck should have
/// kept these consistent.
fn concat_values(kind: ConcatKind, left: &Value, right: &Value) -> Result<Value, RuntimeError> {
    match kind {
        ConcatKind::String => {
            let (Value::String(l), Value::String(r)) = (left, right) else {
                return Err(RuntimeError::TypeMismatch {
                    detail: format!("Concat<String> on `{left}` and `{right}`"),
                });
            };
            let mut out = Vec::with_capacity(l.len() + r.len());
            out.extend_from_slice(l);
            out.extend_from_slice(r);
            Ok(Value::string(out))
        }
        ConcatKind::Binary => {
            let (Value::Binary(l), Value::Binary(r)) = (left, right) else {
                return Err(RuntimeError::TypeMismatch {
                    detail: format!("Concat<Binary> on `{left}` and `{right}`"),
                });
            };
            let mut out = Vec::with_capacity(l.len() + r.len());
            out.extend_from_slice(l);
            out.extend_from_slice(r);
            Ok(Value::binary(out))
        }
        ConcatKind::Bits => {
            let (
                Value::Bits {
                    bytes: lb,
                    bit_length: ll,
                },
                Value::Bits {
                    bytes: rb,
                    bit_length: rl,
                },
            ) = (left, right)
            else {
                return Err(RuntimeError::TypeMismatch {
                    detail: format!("Concat<Bits> on `{left}` and `{right}`"),
                });
            };
            let total = ll + rl;
            let total_bytes = total.div_ceil(8) as usize;
            let mut out = vec![0u8; total_bytes];
            // Copy lhs bits (which are already left-aligned in `lb`)
            // verbatim — the trailing partial byte already has its
            // high bits set and low bits zeroed.
            for (idx, byte) in lb.iter().enumerate() {
                out[idx] = *byte;
            }
            // Append rhs bits starting at bit offset `ll`.
            append_bits(&mut out, *ll, rb, *rl);
            Ok(Value::bits(out, total))
        }
    }
}

/// Append `length` bits from `src` (which is left-aligned with
/// `length` valid bits and possible zero padding in the low bits of
/// its trailing byte) into `dest` starting at bit offset
/// `start_bit`. Helper for [`concat_values`]'s `Bits` arm; mirrors
/// the algorithm the LLVM `__koja_concat_bits` runtime helper
/// runs at native code speed.
fn append_bits(dest: &mut [u8], start_bit: u64, src: &[u8], length: u64) {
    if length == 0 {
        return;
    }
    let shift = (start_bit % 8) as u32;
    let dest_byte_start = (start_bit / 8) as usize;
    if shift == 0 {
        let src_bytes = length.div_ceil(8) as usize;
        dest[dest_byte_start..dest_byte_start + src_bytes].copy_from_slice(&src[..src_bytes]);
        return;
    }
    // Bit-shift each source byte right by `shift`, OR'd into the
    // current dest byte's low bits + the next dest byte's high
    // bits.
    let mut remaining = length;
    let mut src_idx = 0;
    let mut dest_idx = dest_byte_start;
    while remaining > 0 {
        let byte = src[src_idx];
        dest[dest_idx] |= byte >> shift;
        let next_bits = remaining.min(8);
        let consumed_in_low = next_bits + (shift as u64).saturating_sub(0);
        if consumed_in_low > 8 - shift as u64 && dest_idx + 1 < dest.len() {
            dest[dest_idx + 1] |= byte << (8 - shift);
        }
        if remaining > 8 {
            remaining -= 8;
            src_idx += 1;
            dest_idx += 1;
        } else {
            remaining = 0;
        }
    }
}

/// Build a `<<segments>>` literal as a runtime [`Value::Binary`] (when
/// `layout.byte_aligned`) or [`Value::Bits`] (otherwise). Segments
/// are packed in source order at their pre-computed `bit_offset`s;
/// integer / float bytes get endian-shuffled, string segments
/// `memcpy` their payload, sub-byte segments funnel through
/// [`pack_bits_into`] (the eval-side mirror of the
/// `__koja_pack_bits` runtime helper). The buffer is
/// pre-zeroed so unused trailing bits in the last byte stay zero.
fn construct_binary_literal(
    layout: ResolvedBinaryLayout,
    segments: &[LoweredBinarySegment],
    frame: &Frame,
) -> Result<Value, RuntimeError> {
    let total_bytes = layout.total_bits.div_ceil(8) as usize;
    let mut buffer = vec![0u8; total_bytes];

    for segment in segments {
        match segment {
            LoweredBinarySegment::Integer {
                value,
                width,
                endian,
                bit_offset,
                ..
            } => {
                let resolved = lookup(&frame.values, *value)?;
                let int_value = match resolved {
                    Value::Int(n) => n as u64,
                    other => {
                        return Err(RuntimeError::TypeMismatch {
                            detail: format!(
                                "binary literal integer segment expected an Int value; got {other}",
                            ),
                        });
                    }
                };
                pack_integer_segment(&mut buffer, int_value, *width, *endian, *bit_offset);
            }
            LoweredBinarySegment::Float {
                value,
                width,
                endian,
                bit_offset,
            } => {
                let resolved = lookup(&frame.values, *value)?;
                let bits: u64 = match (*width, &resolved) {
                    (32, Value::Float32(v)) => u64::from(v.to_bits()),
                    (32, Value::Float64(v)) => u64::from((*v as f32).to_bits()),
                    (64, Value::Float64(v)) => v.to_bits(),
                    (64, Value::Float32(v)) => f64::from(*v).to_bits(),
                    (w, _) => panic!(
                        "interpreter: BinaryConstruct float segment of width {w} — \
                         seal invariant violation (float widths are 32 or 64)",
                    ),
                };
                pack_integer_segment(&mut buffer, bits, *width, *endian, *bit_offset);
            }
            LoweredBinarySegment::String {
                value,
                byte_length,
                bit_offset,
            } => {
                let resolved = lookup(&frame.values, *value)?;
                let Value::String(bytes) = resolved else {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!(
                            "binary literal string segment expected a String value; got {resolved}",
                        ),
                    });
                };
                debug_assert!(
                    bytes.len() as u64 >= *byte_length,
                    "interpreter: BinaryConstruct string segment carries byte_length {byte_length} \
                     but the runtime String holds {} bytes — typecheck/lower invariant violation",
                    bytes.len(),
                );
                let start_byte = (bit_offset / 8) as usize;
                buffer[start_byte..start_byte + *byte_length as usize]
                    .copy_from_slice(&bytes[..*byte_length as usize]);
            }
        }
    }

    if layout.byte_aligned {
        Ok(Value::binary(buffer))
    } else {
        Ok(Value::bits(buffer, layout.total_bits))
    }
}

/// Pack the low `width` bits of `value` into `buffer` starting at
/// `start_bit`, byte-flipping per `endian`. The byte-aligned fast
/// path collapses to a per-byte `or` (mirrors the LLVM backend's
/// `emit_byte_packing` shape); the sub-byte path delegates to
/// [`pack_bits_into`] so the integer / float arms share one
/// bit-stream packer.
fn pack_integer_segment(
    buffer: &mut [u8],
    value: u64,
    width: u64,
    endian: BinaryEndian,
    start_bit: u64,
) {
    if width == 0 {
        return;
    }
    if start_bit.is_multiple_of(8) && width.is_multiple_of(8) {
        let num_bytes = (width / 8) as usize;
        let start_byte = (start_bit / 8) as usize;
        for i in 0..num_bytes {
            let shift = match endian {
                BinaryEndian::Little => (i as u32) * 8,
                BinaryEndian::Big => ((num_bytes - 1 - i) as u32) * 8,
            };
            buffer[start_byte + i] = (value >> shift) as u8;
        }
        return;
    }
    // Sub-byte: write the low `width` bits MSB-first, mirroring the
    // runtime `__koja_pack_bits` helper. Endianness is
    // meaningless for non-byte-multiple widths in v1, so we only
    // honour the high-order-first convention.
    pack_bits_into(buffer, value, width, start_bit);
}

/// Eval-side mirror of the `__koja_pack_bits` runtime helper:
/// write the low `width` bits of `value` (MSB first) into `buffer`
/// at bit offset `start_bit`. `buffer` is assumed pre-zeroed; we
/// `or` rather than overwrite so adjacent segments that share a
/// byte don't clobber each other.
fn pack_bits_into(buffer: &mut [u8], value: u64, width: u64, start_bit: u64) {
    for i in 0..width {
        let bit = ((value >> (width - 1 - i)) & 1) as u8;
        if bit == 0 {
            continue;
        }
        let bit_pos = start_bit + i;
        let byte = (bit_pos / 8) as usize;
        let bit_in_byte = 7 - (bit_pos % 8) as u32;
        buffer[byte] |= 1 << bit_in_byte;
    }
}

/// Eval-side `BinaryMatch` driver, mirroring the LLVM emission
/// described on [`IRInstruction::BinaryMatch`]: gate on the
/// subject's runtime bit length (equality without a greedy tail,
/// `>=` with one), then test every literal segment and — as a side
/// effect — extract each `BindInt` / `GreedyTail` slice into its
/// pre-declared local slot. Binds happen as segments are walked,
/// matching the LLVM order; a later literal failure leaves earlier
/// binds written, which is unobservable because the arm body only
/// runs when the whole match succeeds.
fn execute_binary_match(
    layout: LoweredBinaryMatchLayout,
    segments: &[LoweredBinaryPattern],
    subject: &Value,
    frame: &mut Frame,
) -> Result<bool, RuntimeError> {
    let (bytes, bit_length) = match subject {
        Value::Binary(b) | Value::String(b) => (b.as_slice(), b.len() as u64 * 8),
        Value::Bits { bytes, bit_length } => (bytes.as_slice(), *bit_length),
        other => {
            return Err(RuntimeError::TypeMismatch {
                detail: format!("binary match expects a Binary/Bits/String subject; got {other}"),
            });
        }
    };
    let length_ok = if layout.has_greedy_tail {
        bit_length >= layout.fixed_bits
    } else {
        bit_length == layout.fixed_bits
    };
    if !length_ok {
        return Ok(false);
    }

    for segment in segments {
        match segment {
            LoweredBinaryPattern::LiteralInt {
                bit_offset,
                endian,
                sign: _,
                value,
                width,
            } => {
                // Compare raw width-truncated bits: a negative
                // signed literal and its two's-complement bit
                // pattern agree under the mask, so the sign
                // modifier doesn't change the test.
                let extracted = extract_integer_segment(bytes, *width, *endian, *bit_offset);
                if extracted != (*value as u64) & width_mask(*width) {
                    return Ok(false);
                }
            }
            LoweredBinaryPattern::LiteralBytes {
                bit_offset,
                bytes: expected,
            } => {
                let start = (*bit_offset / 8) as usize;
                if bytes[start..start + expected.len()] != expected[..] {
                    return Ok(false);
                }
            }
            LoweredBinaryPattern::BindInt {
                bit_offset,
                endian,
                local,
                sign,
                ty: _,
                width,
            } => {
                let extracted = extract_integer_segment(bytes, *width, *endian, *bit_offset);
                frame
                    .locals
                    .insert(*local, Value::Int(sign_interpret(extracted, *width, *sign)));
            }
            LoweredBinaryPattern::Discard { .. } => {}
            LoweredBinaryPattern::GreedyTail {
                bit_offset,
                local,
                ty,
            } => {
                let Some(local) = local else { continue };
                let tail = match ty {
                    // Typecheck guarantees a byte-aligned prefix for
                    // a `Binary` tail.
                    IRType::Binary => Value::binary(&bytes[(*bit_offset / 8) as usize..]),
                    IRType::Bits => Value::bits(
                        extract_bit_range(bytes, *bit_offset, bit_length - *bit_offset),
                        bit_length - *bit_offset,
                    ),
                    other => panic!(
                        "interpreter: binary-match greedy tail typed `{other:?}` — \
                         seal invariant violation (tail is Binary or Bits)",
                    ),
                };
                frame.locals.insert(*local, tail);
            }
        }
    }
    Ok(true)
}

/// Inverse of [`pack_integer_segment`]: read `width` bits at
/// `start_bit` as an unsigned integer, byte-shuffled per `endian`
/// on the byte-aligned fast path, MSB-first on the sub-byte path
/// (where endianness is meaningless in v1).
fn extract_integer_segment(bytes: &[u8], width: u64, endian: BinaryEndian, start_bit: u64) -> u64 {
    if width == 0 {
        return 0;
    }
    if start_bit.is_multiple_of(8) && width.is_multiple_of(8) {
        let num_bytes = (width / 8) as usize;
        let start_byte = (start_bit / 8) as usize;
        let mut value = 0u64;
        for (i, byte) in bytes[start_byte..start_byte + num_bytes].iter().enumerate() {
            let shift = match endian {
                BinaryEndian::Little => (i as u32) * 8,
                BinaryEndian::Big => ((num_bytes - 1 - i) as u32) * 8,
            };
            value |= u64::from(*byte) << shift;
        }
        return value;
    }
    let mut value = 0u64;
    for i in 0..width {
        let bit_pos = start_bit + i;
        let byte = (bit_pos / 8) as usize;
        let bit_in_byte = 7 - (bit_pos % 8) as u32;
        value = (value << 1) | u64::from((bytes[byte] >> bit_in_byte) & 1);
    }
    value
}

/// Reinterpret the raw `width`-bit pattern per the segment's sign
/// modifier: sign-extend when `Signed` and the sign bit is set,
/// zero-extend otherwise. Mirrors the LLVM emission's `sext`/`zext`
/// choice on `BindInt`.
fn sign_interpret(value: u64, width: u64, sign: BinarySign) -> i64 {
    match sign {
        BinarySign::Unsigned => value as i64,
        BinarySign::Signed => {
            if width == 0 || width >= 64 {
                return value as i64;
            }
            let sign_bit = 1u64 << (width - 1);
            if value & sign_bit != 0 {
                (value | !width_mask(width)) as i64
            } else {
                value as i64
            }
        }
    }
}

/// All-ones mask covering the low `width` bits (`u64::MAX` at 64+).
fn width_mask(width: u64) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}

/// Copy `length` bits starting at `start_bit` into a fresh
/// MSB-first, zero-padded byte buffer — the greedy-tail extraction
/// for `Bits`. Byte-aligned starts take the `memcpy` fast path.
fn extract_bit_range(bytes: &[u8], start_bit: u64, length: u64) -> Vec<u8> {
    let byte_count = length.div_ceil(8) as usize;
    if start_bit.is_multiple_of(8) {
        let start = (start_bit / 8) as usize;
        let mut out = bytes[start..start + byte_count].to_vec();
        // Zero any trailing bits past `length` so equality on the
        // resulting `Bits` value stays well-defined.
        if !length.is_multiple_of(8) {
            let last = out.len() - 1;
            out[last] &= !(0xffu8 >> (length % 8));
        }
        return out;
    }
    let mut out = vec![0u8; byte_count];
    for i in 0..length {
        let bit_pos = start_bit + i;
        let bit = (bytes[(bit_pos / 8) as usize] >> (7 - (bit_pos % 8) as u32)) & 1;
        if bit != 0 {
            out[(i / 8) as usize] |= 1 << (7 - (i % 8) as u32);
        }
    }
    out
}

/// Materialize a `ConstValue` as a runtime [`Value`]. Every int
/// width collapses to `Value::Int(i64)` (the seal pass keeps
/// width-mismatched flows out, but the arms stay exhaustive).
fn materialize_const(value: &ConstValue) -> Value {
    match value {
        ConstValue::Binary(bytes) => Value::binary(bytes.clone()),
        ConstValue::Bits { bytes, bit_length } => Value::bits(bytes.clone(), *bit_length),
        ConstValue::Bool(b) => Value::Bool(*b),
        ConstValue::Float32(v) => Value::Float32(*v),
        ConstValue::Float64(v) => Value::Float64(*v),
        ConstValue::Int8(v) => Value::Int(*v as i64),
        ConstValue::Int16(v) => Value::Int(*v as i64),
        ConstValue::Int32(v) => Value::Int(*v as i64),
        ConstValue::Int64(v) => Value::Int(*v),
        ConstValue::String(s) => Value::string(s.as_bytes()),
        ConstValue::UInt8(v) => Value::Int(*v as i64),
        ConstValue::UInt16(v) => Value::Int(*v as i64),
        ConstValue::UInt32(v) => Value::Int(*v as i64),
        ConstValue::UInt64(v) => Value::Int(*v as i64),
        ConstValue::Unit => Value::Unit,
    }
}
