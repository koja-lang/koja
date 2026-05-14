//! Tree-walking interpreter over a sealed [`IRProgram`] / [`IRScript`].
//! Parameterized over a [`CallResolver`] so both IR shapes share the
//! per-instruction execution, frame management, and terminator
//! dispatch code; only callee lookup differs. Operator math lives in
//! [`crate::ops`].

use std::collections::BTreeMap;

use expo_alpha_ir::{
    BinaryEndian, BranchTarget, ConcatKind, ConstValue, EnumPayloadInit, FunctionKind,
    IRBasicBlock, IRBlockId, IRConstantValue, IREnumDecl, IRFunction, IRInstruction, IRLocalId,
    IRProgram, IRScript, IRStructDecl, IRSymbol, IRTerminator, IRType, IRVariantPayload,
    IRVariantTag, LoweredBinarySegment, ResolvedBinaryLayout, ValueId,
};

use crate::error::RuntimeError;
use crate::externs;
use crate::intrinsics;
use crate::ops::{apply_binary_op, apply_unary_op};
use crate::value::{EnumPayload, Value};

pub struct Interpreter;

impl Interpreter {
    /// Execute the project-mode entry function and return its result.
    /// For [`FunctionKind::ProcessEntryWrapper`] entries the interpreter
    /// dispatches through `state.start` / `state.run`: an `Ok` start
    /// chains into `run` and the returned `StopReason` is reported as
    /// a [`Value::Int`] (`StopReason.code()` semantics); an `Err`
    /// start surfaces its embedded `StopReason` the same way. The
    /// interpreter has no host argv, so a `List<String>` config
    /// resolves to an empty list.
    pub fn run_program(program: IRProgram) -> Result<Value, RuntimeError> {
        let entry = program.entry_function();
        if let FunctionKind::ProcessEntryWrapper { state } = &entry.kind {
            return run_process_entry(&program, entry, state);
        }
        execute_function(entry, Vec::new(), &program)
    }

    /// Execute the script-mode implicit body and return its trailing
    /// value. Borrows `script` so the caller can dispatch follow-up
    /// helper calls (e.g. [`Self::format_via_debug`] for the REPL's
    /// inspect-style print) without re-lowering.
    pub fn run_script(script: &IRScript) -> Result<Value, RuntimeError> {
        let mut frame = Frame::new();
        match execute_blocks(&script.blocks, &mut frame, script)? {
            BlockOutcome::Done(value) => Ok(value),
            BlockOutcome::TailRestart(_) => panic!(
                "interpreter: script body produced a `TailCall` terminator — \
                 tail-call rewrite never targets the implicit script body",
            ),
        }
    }

    /// Dispatch the `Debug.format` instance for `value`, returning
    /// the rendered UTF-8 bytes. Mirrors the symbol the alpha IR
    /// lower pass would emit for `value.format()`, so the caller's
    /// output matches what a user-side `IO.puts(value.format())`
    /// would produce. Today's only caller is
    /// [`expo_alpha_shell`]'s REPL inspect line.
    ///
    /// Drives off the runtime [`Value`] shape rather than the
    /// caller's static IR type, because the script's
    /// [`IRScript::return_type`] tracks the trailing expression's
    /// declared return — which is `Unit` for any method whose
    /// signature elides `-> T` (e.g. `Debug.print`), even when the
    /// body's actual trailing expression hands back a richer value
    /// like `Result<Int, String>`. Routing off the live value
    /// resolves this static / dynamic mismatch.
    ///
    /// Returns `None` for shapes where the runtime [`Display`] of
    /// [`Value`] is the right rendering — primitive scalars and the
    /// first-class container shapes ([`Value::List`] / [`Value::Map`]
    /// / [`Value::Set`]) whose `Display` recurses through nested
    /// values' own `Display`. For the container shapes specifically,
    /// this means a `List<Result<Int, String>>` falls back to the
    /// runtime `Display`'s `[Global.Result_$..$.Ok(1)]` rendering —
    /// improving that requires plumbing the element type through to
    /// the caller's render site, a follow-up.
    pub fn format_via_debug(
        script: &IRScript,
        value: Value,
    ) -> Result<Option<Vec<u8>>, RuntimeError> {
        let symbol = match &value {
            Value::Enum { symbol, .. } | Value::Struct { symbol, .. } => {
                expo_alpha_ir::mangling::debug_format_for_symbol(symbol)
            }
            Value::Binary(_)
            | Value::Bits { .. }
            | Value::Bool(_)
            | Value::CPtr(_)
            | Value::Closure { .. }
            | Value::Float32(_)
            | Value::Float64(_)
            | Value::Int(_)
            | Value::List(_)
            | Value::Map(_)
            | Value::Set(_)
            | Value::String(_)
            | Value::Union { .. }
            | Value::Unit => return Ok(None),
        };
        let function =
            script
                .function(symbol.mangled())
                .ok_or_else(|| RuntimeError::TypeMismatch {
                    detail: format!(
                        "format_via_debug: `Debug.format` instance `{}` is not in the IR \
                         — the script's monomorphizer did not specialize it",
                        symbol.mangled(),
                    ),
                })?;
        let result = execute_function(function, vec![value], script)?;
        match result {
            Value::String(bytes) => Ok(Some(bytes)),
            other => Err(RuntimeError::TypeMismatch {
                detail: format!(
                    "format_via_debug: `{}` returned non-String value `{other}` — \
                     Debug.format contract violation",
                    symbol.mangled(),
                ),
            }),
        }
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

/// Drive a [`FunctionKind::ProcessEntryWrapper`] entry under the
/// interpreter: call `state.start(config)`, on `Ok` chain into
/// `state.run`, then hand the resulting `StopReason` to
/// `Global.StopReason.code`. `Err` skips `run` and routes its
/// embedded `StopReason` straight to `code`. The returned value is
/// the integer exit code (`Value::Int`) — analogous to what the
/// LLVM trampoline stores into `__expo_exit_code`.
fn run_process_entry(
    program: &IRProgram,
    entry: &IRFunction,
    state: &IRType,
) -> Result<Value, RuntimeError> {
    let IRType::Struct(state_symbol) = state else {
        return Err(RuntimeError::Unsupported {
            detail: format!(
                "process entry wrapper `{}` declared with non-struct state `{state:?}`",
                entry.symbol,
            ),
        });
    };
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
    let config_value = default_value_for_type(config_type, program)?;

    let start_symbol = format!("{}.start", state_symbol.mangled());
    let start_fn = program
        .function(&start_symbol)
        .ok_or_else(|| RuntimeError::Unsupported {
            detail: format!(
                "process entry wrapper `{}` cannot resolve start method `{start_symbol}`",
                entry.symbol,
            ),
        })?;
    let start_result = execute_function(start_fn, vec![config_value], program)?;
    let stop_reason = match start_result {
        Value::Enum { tag, payload, .. } if tag.0 == 0 => {
            let state_value = take_first_payload_field(payload, &entry.symbol, "Ok")?;
            let run_symbol = format!("{}.run", state_symbol.mangled());
            let run_fn =
                program
                    .function(&run_symbol)
                    .ok_or_else(|| RuntimeError::Unsupported {
                        detail: format!(
                            "process entry wrapper `{}` cannot resolve run method `{run_symbol}`",
                            entry.symbol,
                        ),
                    })?;
            execute_function(run_fn, vec![state_value], program)?
        }
        Value::Enum { tag, payload, .. } if tag.0 == 1 => {
            take_first_payload_field(payload, &entry.symbol, "Err")?
        }
        other => {
            return Err(RuntimeError::Unsupported {
                detail: format!(
                    "process entry wrapper `{}` start() returned a non-Result value `{other:?}`",
                    entry.symbol,
                ),
            });
        }
    };

    let code_symbol = "Global.StopReason.code";
    let code_fn = program
        .function(code_symbol)
        .ok_or_else(|| RuntimeError::Unsupported {
            detail: format!(
                "process entry wrapper `{}` cannot resolve `{code_symbol}`",
                entry.symbol,
            ),
        })?;
    execute_function(code_fn, vec![stop_reason], program)
}

/// Build a fresh interpreter [`Value`] suitable as the entry's config
/// argument. Mirrors the LLVM trampoline's zero-init / argv-build
/// shape: empty structs round-trip as `Value::Struct` with no
/// fields, `List<T>` produces an empty list, and primitive scalars
/// default to their zero element. Anything richer than that needs
/// host argv plumbing the interpreter doesn't have.
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
        IRType::String => Ok(Value::String(Vec::new())),
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

/// Pull the first payload field out of a `Value::Enum` produced by
/// `start` — used to extract either the `Ok(state)` state or the
/// `Err(stop_reason)` reason. Both variants today carry a single
/// positional field on a [`EnumPayload::Tuple`] layout.
fn take_first_payload_field(
    payload: crate::value::EnumPayload,
    function: &IRSymbol,
    variant: &str,
) -> Result<Value, RuntimeError> {
    use crate::value::EnumPayload;
    match payload {
        EnumPayload::Tuple(mut values) if !values.is_empty() => Ok(values.swap_remove(0)),
        EnumPayload::Struct(mut entries) if !entries.is_empty() => Ok(entries.swap_remove(0).1),
        other => Err(RuntimeError::Unsupported {
            detail: format!(
                "process entry wrapper `{function}` start() `{variant}` variant has empty / \
                 unrecognized payload `{other:?}`",
            ),
        }),
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
fn execute_function<R: CallResolver>(
    function: &IRFunction,
    mut args: Vec<Value>,
    resolver: &R,
) -> Result<Value, RuntimeError> {
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
            return intrinsics::dispatch(id, function, &args, resolver);
        }
        FunctionKind::Extern(attrs) => {
            let c_symbol = attrs
                .link_name
                .as_deref()
                .unwrap_or_else(|| function.symbol.last_segment());
            return match externs::dispatch(c_symbol, &args) {
                Some(result) => result,
                None => Err(RuntimeError::ExternNotSupported {
                    symbol: function.symbol.mangled().to_string(),
                }),
            };
        }
        FunctionKind::Closure { .. } => panic!(
            "interpreter: direct `Call` to closure body `{}` — must dispatch via \
             `CallClosure` (seal invariant violation)",
            function.symbol,
        ),
        FunctionKind::SpawnWrapper { .. } => {
            return Err(RuntimeError::Unsupported {
                detail: format!(
                    "spawn wrapper `{}` cannot be invoked directly under the alpha interpreter; \
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
        match execute_blocks(&function.blocks, &mut frame, resolver)? {
            BlockOutcome::Done(value) => return Ok(value),
            BlockOutcome::TailRestart(new_args) => {
                args = new_args;
            }
        }
    }
}

/// Dispatch a [`FunctionKind::Closure`] body with its captured
/// environment. Mirrors [`execute_function`] for `Regular` bodies,
/// but seeds `frame.captures` so [`IRInstruction::LoadCapture`] can
/// index into the env array. `captures.len()` matches the body's
/// `env_layout` (seal invariant).
fn execute_closure_function<R: CallResolver>(
    function: &IRFunction,
    args: Vec<Value>,
    captures: Vec<Value>,
    resolver: &R,
) -> Result<Value, RuntimeError> {
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
    match execute_blocks(&function.blocks, &mut frame, resolver)? {
        BlockOutcome::Done(value) => Ok(value),
        BlockOutcome::TailRestart(_) => panic!(
            "interpreter: closure body `{}` produced a `TailCall` terminator — \
             tail-call rewrite is not enabled for closures yet",
            function.symbol,
        ),
    }
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
fn execute_blocks<R: CallResolver>(
    blocks: &[IRBasicBlock],
    frame: &mut Frame,
    resolver: &R,
) -> Result<BlockOutcome, RuntimeError> {
    let mut current = blocks
        .first()
        .expect("sealed function has at least one basic block")
        .id;
    loop {
        let block = find_block(blocks, current);
        for instruction in &block.instructions {
            execute_instruction(instruction, frame, resolver)?;
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
                        detail: format!("cond_branch expects a Bool condition; got {cond_value}",),
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
}

/// Evaluate `target.args` in the predecessor's value-map and bind
/// the resulting [`Value`]s to the target block's
/// [`expo_alpha_ir::BlockParam::dest`] ids before stepping into the
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

fn find_block(blocks: &[IRBasicBlock], id: IRBlockId) -> &IRBasicBlock {
    blocks
        .iter()
        .find(|b| b.id == id)
        .unwrap_or_else(|| panic!("interpreter: block `{id}` missing — seal invariant violation"))
}

fn execute_instruction<R: CallResolver>(
    instruction: &IRInstruction,
    frame: &mut Frame,
    resolver: &R,
) -> Result<(), RuntimeError> {
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
            let result = execute_function(callee_fn, arg_values, resolver)?;
            frame.values.insert(*dest, result);
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
        // Slot identity comes from `LocalWrite`; `LocalDecl` is a
        // no-op for the interpreter (the LLVM backend uses it to
        // emit an entry-block alloca).
        IRInstruction::DropLocal { .. } => Ok(()),
        // Heap reclamation is handled by the host GC; the IR-level
        // value-keyed drop is a no-op for the interpreter (mirrors
        // [`IRInstruction::DropLocal`] above).
        IRInstruction::DropValue { .. } => Ok(()),
        IRInstruction::LocalDecl { .. } => Ok(()),
        IRInstruction::LocalRead { dest, local, .. } => {
            let value = frame.locals.get(local).cloned().unwrap_or_else(|| {
                panic!(
                    "interpreter: `LocalRead` of `{local}` before its `LocalWrite` — \
                     seal invariant violation",
                )
            });
            frame.values.insert(*dest, value);
            Ok(())
        }
        IRInstruction::LocalWrite {
            local,
            ownership: _,
            value,
        } => {
            let resolved = lookup(&frame.values, *value)?;
            frame.locals.insert(*local, resolved);
            Ok(())
        }
        IRInstruction::MoveOutLocal { dest, local, .. } => {
            let value = frame.locals.remove(local).unwrap_or_else(|| {
                panic!(
                    "interpreter: `MoveOutLocal` on `{local}` before its `LocalWrite` (or \
                     after a prior move) — seal / lower invariant violation",
                )
            });
            frame.values.insert(*dest, value);
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
                    detail: format!("CallClosure expects a Closure receiver; got {callee_value}"),
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
            let result = execute_closure_function(body_fn, arg_values, captures, resolver)?;
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
        IRInstruction::Spawn { wrapper, .. } => Err(RuntimeError::Unsupported {
            detail: format!(
                "`spawn` (wrapper `{wrapper}`) is not supported under the alpha interpreter; \
                 process scheduling lives in the LLVM runtime",
            ),
        }),
        IRInstruction::Receive { .. } => Err(RuntimeError::Unsupported {
            detail: "`receive` is not supported under the alpha interpreter; mailbox dispatch \
                 lives in the LLVM runtime"
                .to_string(),
        }),
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
        IRInstruction::BinaryMatch { .. } => {
            // Binary pattern matching is implemented in the LLVM
            // backend; the alpha interpreter currently skips it
            // because the only consumer (lib/global tests) runs
            // through `--backend=llvm`. Lift to a fatal panic so a
            // mis-routed eval run surfaces immediately rather than
            // silently producing a wrong result.
            panic!(
                "alpha interpreter: IRInstruction::BinaryMatch is only supported by the \
                 LLVM backend — re-run with `--backend=llvm` or extend the interpreter",
            );
        }
    }
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
            Ok(Value::String(out))
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
            Ok(Value::Binary(out))
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
            Ok(Value::Bits {
                bytes: out,
                bit_length: total,
            })
        }
    }
}

/// Append `length` bits from `src` (which is left-aligned with
/// `length` valid bits and possible zero padding in the low bits of
/// its trailing byte) into `dest` starting at bit offset
/// `start_bit`. Helper for [`concat_values`]'s `Bits` arm; mirrors
/// the algorithm the LLVM `__expo_alpha_concat_bits` runtime helper
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
/// `__expo_alpha_pack_bits` runtime helper). The buffer is
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
        Ok(Value::Binary(buffer))
    } else {
        Ok(Value::Bits {
            bytes: buffer,
            bit_length: layout.total_bits,
        })
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
    // runtime `__expo_alpha_pack_bits` helper. Endianness is
    // meaningless for non-byte-multiple widths in v1, so we only
    // honour the high-order-first convention.
    pack_bits_into(buffer, value, width, start_bit);
}

/// Eval-side mirror of the `__expo_alpha_pack_bits` runtime helper:
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

/// Materialize a `ConstValue` as a runtime [`Value`]. Every int
/// width collapses to `Value::Int(i64)` (the seal pass keeps
/// width-mismatched flows out, but the arms stay exhaustive).
fn materialize_const(value: &ConstValue) -> Value {
    match value {
        ConstValue::Binary(bytes) => Value::Binary(bytes.clone()),
        ConstValue::Bits { bytes, bit_length } => Value::Bits {
            bytes: bytes.clone(),
            bit_length: *bit_length,
        },
        ConstValue::Bool(b) => Value::Bool(*b),
        ConstValue::Float32(v) => Value::Float32(*v),
        ConstValue::Float64(v) => Value::Float64(*v),
        ConstValue::Int8(v) => Value::Int(*v as i64),
        ConstValue::Int16(v) => Value::Int(*v as i64),
        ConstValue::Int32(v) => Value::Int(*v as i64),
        ConstValue::Int64(v) => Value::Int(*v),
        ConstValue::String(s) => Value::String(s.as_bytes().to_vec()),
        ConstValue::UInt8(v) => Value::Int(*v as i64),
        ConstValue::UInt16(v) => Value::Int(*v as i64),
        ConstValue::UInt32(v) => Value::Int(*v as i64),
        ConstValue::UInt64(v) => Value::Int(*v as i64),
        ConstValue::Unit => Value::Unit,
    }
}
