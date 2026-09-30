//! Tree-walking interpreter over a sealed [`IRProgram`] / [`IRScript`].
//! Parameterized over a [`CallResolver`] so both IR shapes share the
//! per-instruction execution, frame management, and terminator
//! dispatch code. Only callee lookup differs. Operator math lives in
//! [`crate::ops`].

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
use std::iter::once;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Instant;

use koja_ir::mangling::closure_eq_env_symbol;
use koja_ir::{
    BranchTarget, ConstValue, EnumPayloadInit, FunctionKind, IRBasicBlock, IRBlockId,
    IRConstantValue, IREnumDecl, IRFunction, IRInstruction, IRIntrinsicId, IRLocalId, IRPackage,
    IRProgram, IRScript, IRStructDecl, IRSymbol, IRTerminator, IRType, IRVariantPayload,
    IRVariantTag, ReceiveAfter, ReceiveArm, ReceiveTag, ValueId,
};
use koja_runtime_core::{
    CrashInfo, Driver, ExitNotice, ExitReason, Lifecycle, Priority, Readiness, Tag, Wake,
    duration_from_user_millis,
};

use crate::binary_ops::{
    concat_values, construct_binary_literal, execute_binary_match, extend_unique_bytes,
};
use crate::built_constants;
use crate::error::RuntimeError;
use crate::externs;
use crate::externs::foreign::ForeignTable;
use crate::intrinsics;
use crate::ops::{apply_binary_op, apply_unary_op};
use crate::reactor::EvalReactor;
use crate::scheduler::{
    self, EvalClock, EvalDriver, EvalExecutor, EvalMessage, EvalRuntime, EvalSignals,
    ProcessFuture, YieldOnce, block_on,
};
use crate::value::{EnumPayload, Value};

/// A boxed, lifetime-bound interpreter future. Every function on the
/// suspension-reachable call tree returns one so the tree can `.await`
/// a process park (`receive` / `io_block`) and so the mutual recursion
/// type-checks. A boxed `dyn Future` breaks the otherwise-infinite
/// `impl Future` cycle between the call-tree functions.
type EvalFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, RuntimeError>> + 'a>>;

/// Entry point for running sealed IR in-process. Stateless, since
/// each run installs its own thread-local scheduler state.
pub struct Interpreter;

impl Interpreter {
    /// Execute the project-mode entry and report its exit code as a
    /// [`Value::Int`]. `args` are the program arguments after the
    /// program name. A `Process<List<String>, _, _>` entry receives
    /// them as its config, other config types zero-init via
    /// [`default_value_for_type`].
    pub fn run_program(program: &IRProgram, args: &[String]) -> Result<Value, RuntimeError> {
        let foreign = ForeignTable::resolve(&program.packages, &[]);
        Self::run_program_with(program, args, foreign)
    }

    /// [`Self::run_program`] with a [`ForeignTable`] the caller
    /// resolved, so the driver can settle the backend on the same
    /// resolution the run then uses.
    pub fn run_program_with(
        program: &IRProgram,
        args: &[String],
        foreign: ForeignTable,
    ) -> Result<Value, RuntimeError> {
        let entry = program.entry_function();
        assert!(
            matches!(entry.kind, FunctionKind::ProcessEntryWrapper { .. }),
            "interpreter: program entry `{}` is not a `ProcessEntryWrapper` (seal violation)",
            entry.symbol,
        );
        let args = args.to_vec();
        run_as_entry_process(
            program,
            foreign,
            Box::pin(async move { run_entry_body(program, entry, &args).await }),
        )
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
        let _built = built_constants::install();
        block_on(async {
            build_constants(program).await?;
            execute_function(function, Vec::new(), program).await
        })
    }

    /// Execute the script-mode implicit body and return its trailing
    /// value. Borrows `script` so the caller can re-run or inspect it
    /// without re-lowering.
    ///
    /// Coerces the trailing value to [`Value::Unit`] when the
    /// script's static [`IRScript::return_type`] is `Unit`. See
    /// [`coerce_return`] for the rationale.
    pub fn run_script(script: &IRScript) -> Result<Value, RuntimeError> {
        let foreign = ForeignTable::resolve(&script.packages, &[]);
        Self::run_script_with(script, foreign)
    }

    /// [`Self::run_script`] with a caller-resolved [`ForeignTable`].
    /// The implicit body runs as PID 1 like a program entry, so
    /// top-level `spawn` / `receive` / timers / I/O engage the runtime
    /// instead of tripping the "runtime not installed" guard.
    pub fn run_script_with(
        script: &IRScript,
        foreign: ForeignTable,
    ) -> Result<Value, RuntimeError> {
        run_as_entry_process(script, foreign, Box::pin(run_script_body(script)))
    }
}

/// Run `body` as PID 1 under the shared cooperative driver, with the
/// foreign and built-constant tables installed for the run. Boots the
/// entry process into a fresh core, hands the loop to the driver, and
/// returns the body's result once the driver tears down. The process
/// future has `Output = ()`, so the result travels through `exit_cell`.
/// OS signals drain into PID 1's mailbox only when some `receive` has
/// a `Lifecycle` arm (see [`EvalSignals`]).
fn run_as_entry_process<'a, R: CallResolver>(
    resolver: &'a R,
    foreign: ForeignTable,
    body: EvalFuture<'a, Value>,
) -> Result<Value, RuntimeError> {
    let _foreign = externs::foreign::install(foreign);
    let _built = built_constants::install();
    let runtime = EvalRuntime::new();
    let _guard = scheduler::install_runtime(runtime.clone());
    let main = boot_main(&runtime);

    let exit_cell: Rc<RefCell<Option<Result<Value, RuntimeError>>>> = Rc::new(RefCell::new(None));
    let entry_future: ProcessFuture = {
        let cell = Rc::clone(&exit_cell);
        Box::pin(async move {
            *cell.borrow_mut() = Some(body.await);
        })
    };

    let executor = EvalExecutor::new(Rc::clone(&runtime.core), resolver);
    executor.install_future(main, entry_future);

    let signals = EvalSignals::new(blocks_use_lifecycle(resolver.all_blocks()));
    EvalDriver::new(
        runtime,
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

/// Spawns PID 1 (the entry process) into a fresh cooperative core and
/// enqueues its first wake, the boot [`run_as_entry_process`] performs
/// before handing the loop to the driver.
fn boot_main(runtime: &EvalRuntime) -> koja_runtime_core::Pid {
    let main = runtime
        .core
        .spawn((), None)
        .expect("the entry spawn has no parent to refuse over");
    runtime.ready.borrow_mut().push(Wake {
        pid: main,
        priority: Priority::default(),
    });
    main
}

/// Per-call execution frame. SSA values and local-slot storage live
/// in separate maps so slot identity never collides with SSA
/// identity even though both keys happen to be `u32`. `captures`
/// holds the closure environment array (empty for non-closure
/// frames), which `LoadCapture` indexes into directly.
pub(crate) struct Frame {
    captures: Vec<Value>,
    pub(crate) values: BTreeMap<ValueId, Value>,
    pub(crate) locals: BTreeMap<IRLocalId, Value>,
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
/// driver runs over either IR shape. Function-call resolution and
/// decl lookup share the trait so each `EnumConstruct` arm has a
/// registry-equivalent handle for materializing variant and field
/// names.
pub(crate) trait CallResolver {
    /// Every block the run can execute, across all function bodies
    /// plus the script body when there is one.
    fn all_blocks(&self) -> impl Iterator<Item = &IRBasicBlock>;
    fn built_constant_order(&self) -> &[IRSymbol];
    fn constant_value(&self, mangled: &str) -> Option<&IRConstantValue>;
    fn enum_decl(&self, mangled: &str) -> Option<&IREnumDecl>;
    fn resolve(&self, mangled: &str) -> Option<&IRFunction>;
    fn struct_decl(&self, mangled: &str) -> Option<&IRStructDecl>;
}

impl CallResolver for IRProgram {
    fn all_blocks(&self) -> impl Iterator<Item = &IRBasicBlock> {
        function_blocks(&self.packages)
    }

    fn built_constant_order(&self) -> &[IRSymbol] {
        &self.built_constant_order
    }

    fn constant_value(&self, mangled: &str) -> Option<&IRConstantValue> {
        IRProgram::constant_value(self, mangled)
    }

    fn enum_decl(&self, mangled: &str) -> Option<&IREnumDecl> {
        IRProgram::enum_decl(self, mangled)
    }

    fn resolve(&self, mangled: &str) -> Option<&IRFunction> {
        self.function(mangled)
    }

    fn struct_decl(&self, mangled: &str) -> Option<&IRStructDecl> {
        IRProgram::struct_decl(self, mangled)
    }
}

impl CallResolver for IRScript {
    fn all_blocks(&self) -> impl Iterator<Item = &IRBasicBlock> {
        function_blocks(&self.packages).chain(self.blocks.iter())
    }

    fn built_constant_order(&self) -> &[IRSymbol] {
        &self.built_constant_order
    }

    fn constant_value(&self, mangled: &str) -> Option<&IRConstantValue> {
        IRScript::constant_value(self, mangled)
    }

    fn enum_decl(&self, mangled: &str) -> Option<&IREnumDecl> {
        IRScript::enum_decl(self, mangled)
    }

    fn resolve(&self, mangled: &str) -> Option<&IRFunction> {
        self.function(mangled)
    }

    fn struct_decl(&self, mangled: &str) -> Option<&IRStructDecl> {
        IRScript::struct_decl(self, mangled)
    }
}

/// Every block of every function body across `packages`.
fn function_blocks(packages: &[IRPackage]) -> impl Iterator<Item = &IRBasicBlock> {
    packages
        .iter()
        .flat_map(|package| package.functions.values())
        .flat_map(|function| &function.blocks)
}

/// Run every `Built` constant init in `built_constant_order` and
/// store each value, before the entry body runs. PID 1 does this on
/// both backends, so an init side effect happens once at startup and
/// every later `LoadConst` is a plain read.
async fn build_constants<R: CallResolver>(resolver: &R) -> Result<(), RuntimeError> {
    for symbol in resolver.built_constant_order() {
        let Some(IRConstantValue::Built { init, .. }) = resolver.constant_value(symbol.mangled())
        else {
            panic!(
                "interpreter: built constant order names `{symbol}`, which is not a built \
                 constant (seal invariant violation)"
            );
        };
        let init_fn = resolver.resolve(init.mangled()).unwrap_or_else(|| {
            panic!(
                "interpreter: built constant `{symbol}` init `{init}` missing from IR (seal \
                 invariant violation)"
            )
        });
        let value = execute_function(init_fn, Vec::new(), resolver).await?;
        built_constants::store(symbol.mangled(), value);
    }
    Ok(())
}

/// Outcome of one pass through a function body. `Done` carries the
/// `Return`'s value. `TailRestart` carries the new positional args
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
/// The wrapper itself is a backend ABI shim. The full `start` -> `run` ->
/// `StopReason.code` dispatch lives in the IR-synthesized
/// `<state>.__entry_body` its IR `Call` names, which the interpreter
/// executes directly with the argv-derived (or default) config. The
/// returned [`Value::Int`] is the exit code. Driven by the
/// [`crate::scheduler::EvalExecutor`] (not [`block_on`]) so a `receive`
/// inside it parks against the core mailbox.
async fn run_entry_body<'a>(
    program: &'a IRProgram,
    entry: &'a IRFunction,
    args: &[String],
) -> Result<Value, RuntimeError> {
    build_constants(program).await?;
    let config_type = entry.params.first().map(|p| &p.ty).unwrap_or_else(|| {
        panic!(
            "interpreter: process entry wrapper `{}` has no config parameter (seal invariant \
             violation)",
            entry.symbol,
        )
    });
    let config_value = if is_argv_shaped(config_type) {
        argv_value(args)
    } else {
        default_value_for_type(config_type, program)?
    };
    let body_fn = process_body_of(program, &entry.symbol).unwrap_or_else(|| {
        panic!(
            "interpreter: process entry wrapper `{}` IR body carries no resolvable process-body \
             call (seal invariant violation)",
            entry.symbol,
        )
    });
    execute_function(body_fn, vec![config_value], program).await
}

/// Run the script-mode implicit body as PID 1: walk its blocks to the
/// trailing value, coercing to [`Value::Unit`] when the script's static
/// return type is `Unit`. The async analogue of the former synchronous
/// `run_script`, so a top-level `receive` parks against the core mailbox.
async fn run_script_body(script: &IRScript) -> Result<Value, RuntimeError> {
    build_constants(script).await?;
    let mut frame = Frame::new();
    match execute_blocks(&script.blocks, &mut frame, script).await? {
        BlockOutcome::Done(value) => Ok(coerce_return(value, &script.return_type)),
        BlockOutcome::TailRestart(_) => panic!(
            "interpreter: script body produced a `TailCall` terminator, but \
             tail-call rewrite never targets the implicit script body",
        ),
    }
}

/// Whether any of `blocks` has a `receive` with a `Lifecycle` arm.
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

/// Resolve a process wrapper's body, the [`FunctionKind::Regular`]
/// function its single IR `Call` names. Shared by the entry boot and the
/// `spawn` path: a `ProcessEntryWrapper` / `SpawnWrapper` is a pure ABI
/// shim whose body holds the real `start` -> `run` dispatch. `None` only
/// for a malformed wrapper (seal violation). Callers decide whether that
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
            "interpreter: spawn wrapper `{wrapper}` has no process body (seal invariant violation)"
        )
    });
    Box::pin(async move {
        if let Err(error) = execute_function(body_fn, vec![config], resolver).await {
            scheduler::record_crash(render_process_crash(&error));
        }
    })
}

/// Render a crashed process body's diagnostic and capture its
/// [`CrashInfo`]. Eval has no host stack to walk, so `backtrace` is empty.
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

/// Whether a process-entry config type is `List<String>`, the one
/// shape that receives host argv instead of a zero-init default.
/// Mirrors the LLVM trampoline's `argv_shaped` test.
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
/// argument. Mirrors the LLVM trampoline's zero-init shape. Scalars
/// take their zero element, collections and `Binary` start empty,
/// `CPtr` is null, aggregates zero-init each field or element, and an
/// enum is its tag-0 variant with a zeroed payload. The argv-shaped
/// `List<String>` config never reaches this helper, since
/// [`run_entry_body`] routes it through [`argv_value`] first. Types
/// with no zero `Value` (`Bits`, `Function`, `Indirect`, `Union`)
/// surface [`RuntimeError::Unsupported`].
fn default_value_for_type(ty: &IRType, program: &IRProgram) -> Result<Value, RuntimeError> {
    match ty {
        IRType::Binary => Ok(Value::binary(Vec::new())),
        IRType::Bool => Ok(Value::Bool(false)),
        IRType::CPtr(_) => Ok(Value::CPtr(std::ptr::null_mut())),
        IRType::Enum(symbol) => {
            let decl = program.enum_decl(symbol.mangled()).unwrap_or_else(|| {
                panic!("interpreter: enum `{symbol}` missing from IR (seal invariant violation)")
            });
            let variant = decl.variants.first().unwrap_or_else(|| {
                panic!("interpreter: enum `{symbol}` has no variants to zero-init")
            });
            let payload = match &variant.payload {
                IRVariantPayload::Struct(fields) => {
                    let mut values = Vec::with_capacity(fields.len());
                    for field in fields {
                        let value = default_value_for_type(&field.ir_type, program)?;
                        values.push((field.name.clone(), value));
                    }
                    EnumPayload::struct_fields(values)
                }
                IRVariantPayload::Tuple(types) => {
                    let mut values = Vec::with_capacity(types.len());
                    for element in types {
                        values.push(default_value_for_type(element, program)?);
                    }
                    EnumPayload::tuple(values)
                }
                IRVariantPayload::Unit => EnumPayload::Unit,
            };
            Ok(Value::Enum {
                name: variant.name.clone(),
                payload,
                symbol: symbol.clone(),
                tag: variant.tag,
            })
        }
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
        IRType::List(_) => Ok(Value::List(Rc::new(RefCell::new(Vec::new())))),
        IRType::Map { .. } => Ok(Value::Map(Rc::new(RefCell::new(Vec::new())))),
        IRType::Set(_) => Ok(Value::Set(Rc::new(RefCell::new(Vec::new())))),
        IRType::String => Ok(Value::string(Vec::new())),
        IRType::Struct(symbol) => {
            let decl = program.struct_decl(symbol.mangled()).unwrap_or_else(|| {
                panic!("interpreter: struct `{symbol}` missing from IR (seal invariant violation)")
            });
            let mut fields = Vec::with_capacity(decl.fields.len());
            for field in &decl.fields {
                fields.push(default_value_for_type(&field.ir_type, program)?);
            }
            Ok(Value::Struct {
                symbol: symbol.clone(),
                fields,
            })
        }
        IRType::Tuple(elements) => {
            let mut values = Vec::with_capacity(elements.len());
            for element in elements {
                values.push(default_value_for_type(element, program)?);
            }
            Ok(Value::Tuple(values))
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
/// `Return { Some(id) }` even for void-returning functions, and the
/// LLVM backend collapses it to `ret void`. Without this coercion
/// the interpreter would propagate the trailing temp and callers
/// would see a richer-than-declared runtime shape.
fn coerce_return(value: Value, return_type: &IRType) -> Value {
    if matches!(return_type, IRType::Unit) {
        Value::Unit
    } else {
        value
    }
}

/// Run `function` in a fresh frame with `args` bound to its param
/// `ValueId`s. `@intrinsic` functions route to [`crate::intrinsics`].
/// An [`IRTerminator::TailCall`] surfaces as
/// `BlockOutcome::TailRestart`, which re-seeds the frame and re-enters
/// the same body so the host stack stays flat.
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
                // A shim in the dispatch table wins. Anything else
                // goes through the foreign table the run installed.
                if let Some(result) = externs::dispatch(c_symbol, &args).await {
                    return result;
                }
                return match externs::foreign::call(function, &args) {
                    Some(result) => result,
                    None => Err(RuntimeError::ExternUnresolved {
                        c_name: c_symbol.to_string(),
                        link_lib: attrs.link_lib.clone(),
                        reason: "no foreign table was resolved for this run".to_string(),
                        symbol: function.symbol.mangled().to_string(),
                    }),
                };
            }
            // Acquisition / release glue is a no-op under the interpreter.
            // Host values share `Rc` storage and are reclaimed when the
            // last `Rc` drops, so a clone is a rebind of the argument and
            // a drop returns unit. Eval never executes a glue body.
            FunctionKind::CloneGlue | FunctionKind::DeepCopyGlue => {
                return Ok(args.into_iter().next().unwrap_or(Value::Unit));
            }
            FunctionKind::DropGlue => return Ok(Value::Unit),
            FunctionKind::Closure { .. } => panic!(
                "interpreter: direct `Call` to closure body `{}`, which must dispatch via \
             `CallClosure` (seal invariant violation)",
                function.symbol,
            ),
            FunctionKind::EqClosureGlue { .. } => panic!(
                "interpreter: direct `Call` to `$eq_env$` glue `{}`, which must dispatch via \
             `ClosureEquals` (seal invariant violation)",
                function.symbol,
            ),
            // The env glue siblings exist only to back the LLVM env block
            // ABI (teardown via the header's `drop_fn`, process-boundary
            // copy via `copy_fn`). The interpreter's `Value::Closure`
            // carries its captures by value and is reclaimed by the host
            // GC, so it never calls (or even references) either.
            FunctionKind::CopyClosureGlue { .. } => panic!(
                "interpreter: `$copy_env$` env deep-copy glue `{}` is LLVM-only; eval copies \
             closures structurally and never invokes it",
                function.symbol,
            ),
            FunctionKind::DropClosureGlue { .. } => panic!(
                "interpreter: `$drop_env$` capture-release glue `{}` is LLVM-only; eval reclaims \
             closures via the host GC and never invokes it",
                function.symbol,
            ),
            FunctionKind::SpawnWrapper { .. } => panic!(
                "interpreter: direct `Call` to spawn wrapper `{}`, which must dispatch via \
             `Spawn` (seal invariant violation)",
                function.symbol,
            ),
            FunctionKind::ProcessEntryWrapper { .. } => panic!(
                "interpreter: direct `Call` to process entry wrapper `{}`, which only \
             `Interpreter::run_program` dispatches, through state.start / state.run (seal \
             invariant violation)",
                function.symbol,
            ),
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

/// Dispatch a [`FunctionKind::Closure`] body (or its `$eq_env$`
/// glue) with its captured environment. Mirrors [`execute_function`]
/// for `Regular` bodies, but seeds `frame.captures` so
/// [`IRInstruction::LoadCapture`] can index into the env array.
/// `captures.len()` matches the body's `env_layout` (seal invariant).
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
            FunctionKind::Closure { env_layout } | FunctionKind::EqClosureGlue { env_layout } => {
                env_layout
            }
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
                "interpreter: closure body `{}` produced a `TailCall` terminator, but \
             tail-call rewrite is not enabled for closures yet",
                function.symbol,
            ),
        }
    })
}

/// The BEAM rule behind [`IRInstruction::ClosureEquals`]: same body
/// symbol (the eval stand-in for the LLVM `site_id`), then equal
/// captures. Capture comparison runs the body's `$eq_env$` glue with
/// the lhs captures as the frame and the rhs closure as its one
/// param, so eval and LLVM execute the same user `equals?` bodies.
async fn closures_equal<R: CallResolver>(
    lhs: Value,
    rhs: Value,
    resolver: &R,
) -> Result<bool, RuntimeError> {
    let (lhs_body, lhs_captures) = expect_closure(lhs, "ClosureEquals")?;
    let (rhs_body, rhs_captures) = expect_closure(rhs.clone(), "ClosureEquals")?;
    if lhs_body != rhs_body {
        return Ok(false);
    }
    if lhs_captures.is_empty() && rhs_captures.is_empty() {
        return Ok(true);
    }
    let eq_symbol = closure_eq_env_symbol(&lhs_body);
    let eq_glue = resolver.resolve(eq_symbol.mangled()).unwrap_or_else(|| {
        panic!(
            "interpreter: closure body `{lhs_body}` has captures but no `$eq_env$` glue \
         (lowering invariant violation)",
        )
    });
    match execute_closure_function(eq_glue, vec![rhs], lhs_captures, resolver).await? {
        Value::Bool(equal) => Ok(equal),
        other => Err(RuntimeError::TypeMismatch {
            detail: format!("`$eq_env$` glue must return Bool, got {other}"),
        }),
    }
}

fn expect_closure(value: Value, instruction: &str) -> Result<(IRSymbol, Vec<Value>), RuntimeError> {
    match value {
        Value::Closure { body, captures } => Ok((body, captures)),
        other => Err(RuntimeError::TypeMismatch {
            detail: format!("{instruction} expects a Closure operand, got {other}"),
        }),
    }
}

/// Drop the dead registers that would defeat a consuming site's
/// uniqueness gate. An eval register owns an `Rc` clone until the
/// frame ends, so two kinds are released here:
///
/// - Registers defined at or after the site in this block. They hold
///   values from an earlier pass over the block that nothing reads
///   before they are redefined.
/// - When the block exits the frame, every register that shares the
///   receiver's storage and that nothing from the site onward reads.
fn release_dead_registers<R: CallResolver>(
    instruction: &IRInstruction,
    rest: &[IRInstruction],
    terminator: &IRTerminator,
    frame: &mut Frame,
    resolver: &R,
) {
    let Some(receiver) = consuming_receiver(instruction, resolver) else {
        return;
    };
    for defined in once(instruction)
        .chain(rest)
        .filter_map(IRInstruction::dest)
    {
        frame.values.remove(&defined);
    }
    // Only a frame exit ends every register's life at once. A branch
    // keeps the frame, and its targets may still read the registers.
    if matches!(
        terminator,
        IRTerminator::Return { .. } | IRTerminator::TailCall { .. }
    ) {
        release_exit_registers(receiver, once(instruction).chain(rest), terminator, frame);
    }
}

/// Release every register holding `receiver`'s storage that neither
/// `remaining` (the site and everything after it) nor the terminator
/// reads.
fn release_exit_registers<'a>(
    receiver: ValueId,
    remaining: impl Iterator<Item = &'a IRInstruction> + Clone,
    terminator: &IRTerminator,
    frame: &mut Frame,
) {
    let Some(receiver_value) = frame.values.get(&receiver).cloned() else {
        return;
    };
    let dead: Vec<ValueId> = frame
        .values
        .iter()
        .filter(|(id, value)| {
            **id != receiver
                && value.shares_storage(&receiver_value)
                && !terminator.uses_value(**id)
                && !remaining
                    .clone()
                    .any(|instruction| instruction.uses_value(**id))
        })
        .map(|(id, _)| *id)
        .collect();
    for id in dead {
        frame.values.remove(&id);
    }
}

/// The receiver a consuming site takes over, or `None` for any other
/// instruction.
fn consuming_receiver<R: CallResolver>(
    instruction: &IRInstruction,
    resolver: &R,
) -> Option<ValueId> {
    match instruction {
        IRInstruction::Call { callee, args, .. } => {
            let callee_fn = resolver.resolve(callee.mangled())?;
            let consuming = matches!(
                callee_fn.kind,
                FunctionKind::Intrinsic(IRIntrinsicId::Consuming(_))
            );
            consuming.then(|| args.first().copied()).flatten()
        }
        IRInstruction::Concat {
            consumes_lhs: true,
            lhs,
            ..
        } => Some(*lhs),
        _ => None,
    }
}

/// Drive a function body starting at `blocks[0]` until a `Return`
/// or `TailCall` exits. The frame is shared across every block, and
/// there is no step cap.
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
            for (index, instruction) in block.instructions.iter().enumerate() {
                // `Receive` transfers control to an arm (or after) body
                // block instead of defining a value (lowering places it
                // last in its block with an `Unreachable` terminator),
                // so it dispatches here rather than in
                // `execute_instruction`.
                if let IRInstruction::Receive { after, arms, .. } = instruction {
                    current = execute_receive(arms, after.as_ref(), frame, resolver).await?;
                    continue 'blocks;
                }
                release_dead_registers(
                    instruction,
                    &block.instructions[index + 1..],
                    &block.terminator,
                    frame,
                    resolver,
                );
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
                                "cond_branch expects a Bool condition, got {cond_value}",
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
/// target. Seal asserts arg/param arity match, so a length mismatch
/// panics. Args are looked up before bindings are inserted so a
/// hypothetical self-loop's arg list reads the pre-edge values, not
/// the new param bindings.
fn bind_block_params(
    target: &BranchTarget,
    blocks: &[IRBasicBlock],
    values: &mut BTreeMap<ValueId, Value>,
) -> Result<(), RuntimeError> {
    let target_block = find_block(blocks, target.block);
    if target.args.len() != target_block.params.len() {
        panic!(
            "interpreter: branch to `{}` passes {} arg(s) but target declares {} param(s) \
             (seal invariant violation)",
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
/// delivered message beats an already-expired timeout, matching the
/// runtime's message-before-timeout priority. A `receive` with no
/// matching delivery and no `after` parks indefinitely, exactly as the
/// native runtime does (a genuine deadlock is a program bug).
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
                        detail: format!("receive `after` expects an Int timeout, got {value}"),
                    });
                };
                Ok(Instant::now() + duration_from_user_millis(ms))
            })
            .transpose()?;

        let pid = scheduler::current_pid();
        loop {
            if let Some(message) = scheduler::pop_received(pid)
                && let Some(block) = dispatch_received(message, arms, frame, resolver)
            {
                if deadline.is_some() {
                    scheduler::clear_deadline(pid);
                }
                return Ok(block);
            }
            if let Some(deadline) = deadline
                && Instant::now() >= deadline
            {
                scheduler::clear_deadline(pid);
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
/// traffic binds an `(M, Option<ReplyTo<R>>)` built from the arm's
/// payload type. Lifecycle traffic binds the `Lifecycle` enum value.
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
        // The scheduler delivers fully-built event values (the reactor's
        // `IOReady` enum via [`build_io_ready_value`], the monitor
        // machinery's `ExitSignal` struct via [`build_exit_signal_value`]).
        // Bind them into the synthesized arm, whose body reshapes them
        // into the business tuple.
        Tag::ExitSignal => {
            let arm = arms.iter().find(|arm| arm.tag == ReceiveTag::ExitSignal)?;
            frame.locals.insert(arm.payload_local, message.value);
            Some(arm.body)
        }
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
/// `variant` is the wire byte `koja_runtime::signals::drain` documents.
fn lifecycle_value<R: CallResolver>(arm: &ReceiveArm, variant: i64, resolver: &R) -> Value {
    let IRType::Enum(symbol) = &arm.payload_type else {
        panic!(
            "interpreter: lifecycle receive arm payload type is not an enum, got `{:?}` \
             (seal invariant violation)",
            arm.payload_type,
        );
    };
    let event = Lifecycle::from_index(variant).unwrap_or_else(|| {
        panic!("interpreter: lifecycle wire byte {variant} names no `Lifecycle` event")
    });
    let decl = resolver.enum_decl(symbol.mangled()).unwrap_or_else(|| {
        panic!("interpreter: enum `{symbol}` missing from IR (seal invariant violation)")
    });
    let variant_name = event.variant_name();
    let variant_decl = decl
        .variants
        .iter()
        .find(|decl_variant| decl_variant.name == variant_name)
        .unwrap_or_else(|| {
            panic!(
                "interpreter: `{symbol}` has no `{variant_name}` variant (seal invariant violation)"
            )
        });
    Value::Enum {
        name: variant_decl.name.clone(),
        payload: EnumPayload::Unit,
        symbol: symbol.clone(),
        tag: variant_decl.tag,
    }
}

/// Mangled symbol of the kernel `IO.Ready` enum (`lib/global/src/io.koja`).
/// Non-generic, so its symbol is the bare package-qualified name, the
/// same constant the `koja-ir` `deliver_io_ready` elaborate pass keys on.
const IO_READY_SYMBOL: &str = "Global.IO.Ready";

/// Materialize the `IOReady.{Read,Write,Error}(Fd)` value the reactor
/// delivers to a `Fd.watch` owner. Built at send time (the driver's
/// readiness pass) so the receiver's synthesized `ReceiveTag::IOReady` arm
/// just binds it. `readiness` selects the variant. `fd` fills the wrapped
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
        panic!(
            "interpreter: kernel enum `{IO_READY_SYMBOL}` missing from IR, yet a watched fd fired"
        )
    });
    let variant = decl
        .variants
        .iter()
        .find(|variant| variant.name == variant_name)
        .unwrap_or_else(|| {
            panic!("interpreter: `{IO_READY_SYMBOL}` has no `{variant_name}` variant (seal invariant violation)")
        });
    let IRVariantPayload::Tuple(types) = &variant.payload else {
        panic!(
            "interpreter: `IOReady.{variant_name}` payload is not a tuple (seal invariant violation)"
        );
    };
    let [IRType::Struct(fd_symbol)] = types.as_slice() else {
        panic!(
            "interpreter: `IOReady.{variant_name}` payload is not a single `Fd` struct (seal invariant violation)"
        );
    };
    Value::Enum {
        name: variant_name.to_string(),
        payload: EnumPayload::tuple(vec![Value::Struct {
            symbol: fd_symbol.clone(),
            fields: vec![Value::Int(i64::from(fd))],
        }]),
        symbol: decl.symbol.clone(),
        tag: variant.tag,
    }
}

/// Mangled symbol of the stdlib `Process.ExitSignal` struct, the same
/// constant the `koja-ir` `deliver_exit_signal` elaborate pass keys on.
const EXIT_SIGNAL_SYMBOL: &str = "Global.Process.ExitSignal";

/// Materialize the `Process.ExitSignal{ pid, reason }` value for one
/// staged exit notice, mirroring the native wire payload.
pub(crate) fn build_exit_signal_value<R: CallResolver>(resolver: &R, notice: &ExitNotice) -> Value {
    let decl = resolver.struct_decl(EXIT_SIGNAL_SYMBOL).unwrap_or_else(|| {
        panic!(
            "interpreter: stdlib struct `{EXIT_SIGNAL_SYMBOL}` missing from IR, \
             yet a monitor fired"
        )
    });
    let [pid_field, reason_field] = decl.fields.as_slice() else {
        panic!("interpreter: `{EXIT_SIGNAL_SYMBOL}` is not `{{ pid, reason }}`-shaped");
    };
    let IRType::Struct(pid_symbol) = &pid_field.ir_type else {
        panic!("interpreter: `ExitSignal.pid` is not a `Pid` struct");
    };
    let IRType::Enum(reason_symbol) = &reason_field.ir_type else {
        panic!("interpreter: `ExitSignal.reason` is not an `ExitReason` enum");
    };
    let pid = Value::Struct {
        symbol: pid_symbol.clone(),
        fields: vec![Value::Int(notice.target)],
    };
    let reason = build_exit_reason_value(resolver, reason_symbol, notice);
    Value::Struct {
        symbol: decl.symbol.clone(),
        fields: vec![pid, reason],
    }
}

/// Materialize the `ExitReason` enum value for a staged notice from
/// the core [`ExitReason`].
fn build_exit_reason_value<R: CallResolver>(
    resolver: &R,
    reason_symbol: &IRSymbol,
    notice: &ExitNotice,
) -> Value {
    let decl = resolver
        .enum_decl(reason_symbol.mangled())
        .unwrap_or_else(|| {
            panic!("interpreter: enum `{reason_symbol}` missing from IR (seal invariant violation)")
        });
    let variant_name = notice.reason.variant_name();
    let variant = decl
        .variants
        .iter()
        .find(|decl_variant| decl_variant.name == variant_name)
        .unwrap_or_else(|| {
            panic!(
                "interpreter: `{reason_symbol}` has no `{variant_name}` variant \
                 (seal invariant violation)"
            )
        });
    let payload = if notice.reason == ExitReason::Crashed {
        let IRVariantPayload::Tuple(types) = &variant.payload else {
            panic!("interpreter: `ExitReason.Crashed` payload is not a tuple");
        };
        let [IRType::Struct(crash_symbol)] = types.as_slice() else {
            panic!("interpreter: `ExitReason.Crashed` payload is not a single `CrashInfo` struct");
        };
        let crash_info = notice.crash_info.clone().unwrap_or_default();
        EnumPayload::tuple(vec![Value::Struct {
            symbol: crash_symbol.clone(),
            fields: vec![
                Value::String(Rc::new(crash_info.message.into_bytes())),
                Value::String(Rc::new(crash_info.backtrace.into_bytes())),
            ],
        }])
    } else {
        EnumPayload::Unit
    };
    Value::Enum {
        name: variant.name.clone(),
        payload,
        symbol: decl.symbol.clone(),
        tag: variant.tag,
    }
}

fn find_block(blocks: &[IRBasicBlock], id: IRBlockId) -> &IRBasicBlock {
    blocks
        .iter()
        .find(|b| b.id == id)
        .unwrap_or_else(|| panic!("interpreter: block `{id}` missing (seal invariant violation)"))
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
            IRInstruction::BinaryOp {
                dest,
                lhs,
                op,
                operand_ty,
                rhs,
            } => {
                let lhs_value = lookup(&frame.values, *lhs)?;
                let rhs_value = lookup(&frame.values, *rhs)?;
                let result = apply_binary_op(*op, operand_ty, lhs_value, rhs_value)?;
                frame.values.insert(*dest, result);
                Ok(())
            }
            IRInstruction::Call { dest, callee, args } => {
                let callee_fn = resolver.resolve(callee.mangled()).unwrap_or_else(|| {
                    panic!(
                        "interpreter: callee `{callee}` missing from IR \
                     (seal invariant violation)",
                    )
                });
                // A consuming twin's receiver value is dead after the
                // call (consume fusion proved it), so move its register
                // into the args instead of cloning. When that leaves the
                // backing storage uniquely held, the twin mutates it in
                // place instead of copying.
                let consuming = matches!(
                    callee_fn.kind,
                    FunctionKind::Intrinsic(IRIntrinsicId::Consuming(_))
                );
                let mut arg_values = Vec::with_capacity(args.len());
                for (index, arg) in args.iter().enumerate() {
                    let value = if consuming && index == 0 {
                        frame
                            .values
                            .remove(arg)
                            .ok_or(RuntimeError::ValueUndefined { id: *arg })?
                    } else {
                        lookup(&frame.values, *arg)?
                    };
                    arg_values.push(value);
                }
                let result = execute_function(callee_fn, arg_values, resolver).await?;
                frame.values.insert(*dest, result);
                Ok(())
            }
            // A `Clone` is a rebind. `lookup` already bumped the `Rc`, and
            // sharing is safe because every mutation goes through a
            // uniqueness check or builds a fresh value. `DeepCopy` (the
            // process-boundary copy) gets the same treatment for the same
            // reason.
            IRInstruction::Clone { dest, source, .. }
            | IRInstruction::DeepCopy { dest, source, .. } => {
                let value = lookup(&frame.values, *source)?;
                frame.values.insert(*dest, value);
                Ok(())
            }
            IRInstruction::Concat {
                consumes_lhs,
                dest,
                kind,
                lhs,
                rhs,
            } => {
                // A consumed lhs is dead after the concat, so move its
                // register out and extend the buffer in place when
                // that leaves it uniquely held. Same uniqueness gate as
                // the consuming collection twins.
                let right = lookup(&frame.values, *rhs)?;
                let result = if *consumes_lhs {
                    let left = frame
                        .values
                        .remove(lhs)
                        .ok_or(RuntimeError::ValueUndefined { id: *lhs })?;
                    match extend_unique_bytes(left, &right) {
                        Ok(extended) => extended,
                        Err(left) => concat_values(*kind, &left, &right)?,
                    }
                } else {
                    concat_values(*kind, &lookup(&frame.values, *lhs)?, &right)?
                };
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
                    "interpreter: LoadConst `{}` missing from pooled constants (seal invariant violation)",
                    const_id.mangled(),
                )
            });
                let value = match pooled {
                    // PID 1 ran every init before user code started
                    // (see `build_constants`), so this is a plain read.
                    IRConstantValue::Built { .. } => built_constants::value(const_id.mangled()),
                    _ => materialize_pooled_constant(pooled, resolver)?,
                };
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
                        detail: format!("EnumPayloadFieldGet expects an Enum receiver, got {base}"),
                    });
                };
                if actual_tag != *tag {
                    panic!(
                        "interpreter: EnumPayloadFieldGet expected tag {tag} but value carries \
                     tag {actual_tag}, so the match driver failed to gate on a tag check",
                    );
                }
                let field = match &payload {
                    EnumPayload::Tuple(values) => values
                        .get(*payload_index as usize)
                        .cloned()
                        .unwrap_or_else(|| {
                            panic!(
                                "interpreter: EnumPayloadFieldGet tuple index {payload_index} \
                             out of range (seal invariant violation)",
                            )
                        }),
                    EnumPayload::Struct(fields) => fields
                        .get(*payload_index as usize)
                        .map(|(_, value)| value.clone())
                        .unwrap_or_else(|| {
                            panic!(
                                "interpreter: EnumPayloadFieldGet struct index {payload_index} \
                             out of range (seal invariant violation)",
                            )
                        }),
                    EnumPayload::Unit => panic!(
                        "interpreter: EnumPayloadFieldGet on a Unit variant (seal invariant violation)",
                    ),
                };
                frame.values.insert(*dest, field);
                Ok(())
            }
            IRInstruction::EnumTagGet { dest, value, .. } => {
                let base = lookup(&frame.values, *value)?;
                let Value::Enum { tag, .. } = base else {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!("EnumTagGet expects an Enum receiver, got {base}"),
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
                        detail: format!("field_get expects a Struct receiver, got {base_value}",),
                    });
                };
                let field = fields
                    .into_iter()
                    .nth(*field_index as usize)
                    .unwrap_or_else(|| {
                        panic!(
                            "interpreter: FieldGet index {field_index} out of range \
                         (seal invariant violation)",
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
                        detail: format!("field_set expects a Struct receiver, got {base_value}",),
                    });
                };
                let new_field = lookup(&frame.values, *value)?;
                let slot = fields.get_mut(*field_index as usize).unwrap_or_else(|| {
                    panic!(
                        "interpreter: FieldSet index {field_index} out of range (seal invariant \
                     violation)",
                    )
                });
                *slot = new_field;
                frame.values.insert(*dest, Value::Struct { fields, symbol });
                Ok(())
            }
            // Drop the slot's `Rc` so the consuming site that follows
            // sees a unique value.
            IRInstruction::ConsumeLocal { local } => {
                frame.locals.insert(*local, Value::Unit);
                Ok(())
            }
            IRInstruction::DropLocal { .. } => Ok(()),
            IRInstruction::DropValue { .. } => Ok(()),
            IRInstruction::IndirectPresent { base, dest, .. } => {
                let base = lookup(&frame.values, *base)?;
                frame
                    .values
                    .insert(*dest, Value::Bool(!matches!(base, Value::Unit)));
                Ok(())
            }
            // A `Unit` placeholder so a never-written slot (the
            // payload local of an untaken receive arm) still reads at
            // scope exit.
            IRInstruction::LocalDecl { local, .. } => {
                frame.locals.insert(*local, Value::Unit);
                Ok(())
            }
            IRInstruction::LocalRead { dest, local, .. } => {
                let value = frame.locals.get(local).cloned().unwrap_or_else(|| {
                    panic!(
                        "interpreter: `LocalRead` of `{local}` before its `LocalDecl` \
                     (seal invariant violation)",
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
            IRInstruction::TupleGet {
                base,
                dest,
                element_type: _,
                index,
            } => {
                let base_value = lookup(&frame.values, *base)?;
                let Value::Tuple(elements) = base_value else {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!("tuple_get expects a Tuple receiver, got {base_value}"),
                    });
                };
                let element = elements
                    .into_iter()
                    .nth(*index as usize)
                    .unwrap_or_else(|| {
                        panic!(
                            "interpreter: TupleGet index {index} out of range \
                         (seal invariant violation)",
                        )
                    });
                frame.values.insert(*dest, element);
                Ok(())
            }
            IRInstruction::TupleInit {
                dest,
                elements,
                ty: _,
            } => {
                let mut materialized = Vec::with_capacity(elements.len());
                for element in elements {
                    materialized.push(lookup(&frame.values, *element)?);
                }
                frame.values.insert(*dest, Value::Tuple(materialized));
                Ok(())
            }
            IRInstruction::UnaryOp {
                dest,
                op,
                operand,
                operand_ty,
            } => {
                let operand_value = lookup(&frame.values, *operand)?;
                let result = apply_unary_op(*op, operand_ty, operand_value)?;
                frame.values.insert(*dest, result);
                Ok(())
            }
            IRInstruction::CallClosure {
                args,
                callee,
                dest,
                param_types: _,
                result_ty: _,
            } => {
                let callee_value = lookup(&frame.values, *callee)?;
                let Value::Closure { body, captures } = callee_value else {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!(
                            "CallClosure expects a Closure receiver, got {callee_value}"
                        ),
                    });
                };
                let mut arg_values = Vec::with_capacity(args.len());
                for arg in args {
                    arg_values.push(lookup(&frame.values, *arg)?);
                }
                let body_fn = resolver.resolve(body.mangled()).unwrap_or_else(|| {
                    panic!(
                        "interpreter: closure body `{body}` missing from IR \
                     (seal invariant violation)",
                    )
                });
                let result =
                    execute_closure_function(body_fn, arg_values, captures, resolver).await?;
                frame.values.insert(*dest, result);
                Ok(())
            }
            IRInstruction::ClosureEquals { dest, lhs, rhs, .. } => {
                let lhs_value = lookup(&frame.values, *lhs)?;
                let rhs_value = lookup(&frame.values, *rhs)?;
                let equal = closures_equal(lhs_value, rhs_value, resolver).await?;
                frame.values.insert(*dest, Value::Bool(equal));
                Ok(())
            }
            IRInstruction::LoadCaptureOf {
                capture_index,
                closure,
                dest,
                ty: _,
            } => {
                let (_, captures) =
                    expect_closure(lookup(&frame.values, *closure)?, "LoadCaptureOf")?;
                let value = captures
                    .into_iter()
                    .nth(*capture_index as usize)
                    .unwrap_or_else(|| {
                        panic!(
                            "interpreter: LoadCaptureOf index {capture_index} out of range \
                         (seal invariant violation)",
                        )
                    });
                frame.values.insert(*dest, value);
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
                            "interpreter: LoadCapture index {capture_index} out of range, \
                         env has {} entries (seal invariant violation)",
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
            // widen is a pass-through. Only `Float32 -> Float64`
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
                // stable for the returned `Ref`) and queue the spawn request.
                // The executor builds and installs the child's future after
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
                        detail: format!("ProcessExit expects an Int reason, got {reason}"),
                    });
                };
                scheduler::process_exit(reason);
                Ok(())
            }
            IRInstruction::SetPriority { tag } => {
                let level = lookup(&frame.values, *tag)?;
                let Value::Int(level) = level else {
                    return Err(RuntimeError::TypeMismatch {
                        detail: format!("SetPriority expects an Int tag, got {level}"),
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
                "interpreter: `Receive` reached `execute_instruction`, but `execute_blocks` \
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
                        "interpreter: UnionWrap target IRType is not Union, got `{ty:?}` \
                     (seal invariant violation)",
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
                        detail: format!("UnionTagGet expects a Union receiver, got {base}"),
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
                        detail: format!("UnionPayloadGet expects a Union receiver, got {base}"),
                    });
                };
                if actual_tag != *member_index {
                    panic!(
                        "interpreter: UnionPayloadGet expected member-index {member_index} but value \
                     carries tag {actual_tag}, so the match driver failed to gate on a tag check",
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

/// Read a register. The clone is an `Rc` bump for heap-backed
/// values, so the register and the result share storage until one
/// of them is released.
pub(crate) fn lookup(
    values: &BTreeMap<ValueId, Value>,
    id: ValueId,
) -> Result<Value, RuntimeError> {
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
        IRConstantValue::Built { init, .. } => panic!(
            "interpreter: built constant with init `{init}` nested in a static pool entry \
             (IR lowering invariant violation)",
        ),
        IRConstantValue::Primitive(inner) => Ok(materialize_const(inner)),
        IRConstantValue::EnumVariant { tag, ty } => {
            let decl = resolver.enum_decl(ty.mangled()).unwrap_or_else(|| {
                panic!(
                    "interpreter: pooled enum `{}` missing from IR (seal invariant violation)",
                    ty.mangled(),
                )
            });
            let variant = decl.variants.get(usize::from(tag.0)).unwrap_or_else(|| {
                panic!(
                    "interpreter: pooled EnumVariant `{}` references tag {:?} past {} variants \
                         (seal invariant violation)",
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
/// at `tag.0` (seal asserts the tag is in range and matches the
/// payload shape), and zips the init values with the variant's
/// declared shape into an [`EnumPayload`].
fn materialize_enum<R: CallResolver>(
    symbol: &IRSymbol,
    tag: IRVariantTag,
    payload: &EnumPayloadInit,
    frame: &Frame,
    resolver: &R,
) -> Result<Value, RuntimeError> {
    let decl = resolver.enum_decl(symbol.mangled()).unwrap_or_else(|| {
        panic!(
            "interpreter: enum `{symbol}` missing from IR \
             (seal invariant violation)",
        )
    });
    let variant = decl.variants.get(usize::from(tag.0)).unwrap_or_else(|| {
        panic!(
            "interpreter: EnumConstruct on `{symbol}` references tag {tag} but the decl only \
             declares {} variant(s) (seal invariant violation)",
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
            EnumPayload::tuple(values)
        }
        (EnumPayloadInit::Struct(inits), IRVariantPayload::Struct(declared)) => {
            let mut fields = Vec::with_capacity(inits.len());
            for (init, decl_field) in inits.iter().zip(declared.iter()) {
                let value = lookup(&frame.values, init.value)?;
                fields.push((decl_field.name.clone(), value));
            }
            EnumPayload::struct_fields(fields)
        }
        (init, declared) => panic!(
            "interpreter: EnumConstruct payload shape mismatch on `{symbol}.{}`, \
             declared {declared:?} but supplied {init:?} (seal invariant violation)",
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

/// Materialize a `ConstValue` as a runtime [`Value`].
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
