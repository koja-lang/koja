# koja-ir-eval

Tree-walking interpreter built to the
[`COMPILER-NORTHSTAR.md`](../../design/COMPILER-NORTHSTAR.md) contract.
Consumes the sealed `IRProgram` / `IRScript` produced by `koja-ir`. It is
the peer of `koja-ir-llvm` and must agree with it on every observable
result.

## Public surface

```rust
pub struct Interpreter;

impl Interpreter {
    pub fn run_program(program: &IRProgram, args: &[String]) -> Result<Value, RuntimeError>;
    pub fn run_program_with(program: &IRProgram, args: &[String], foreign: ForeignTable) -> Result<Value, RuntimeError>;
    pub fn run_function(program: &IRProgram, mangled: &str) -> Result<Value, RuntimeError>;
    pub fn run_script(script: &IRScript) -> Result<Value, RuntimeError>;
    pub fn run_script_with(script: &IRScript, foreign: ForeignTable) -> Result<Value, RuntimeError>;
}
```

`run_program` boots the project entry `Process` as PID 1 on the
cooperative scheduler and returns its exit code as `Value::Int`. `args`
are the program arguments after the program name. `run_script` runs the
implicit script body as PID 1 the same way and returns the value of its
trailing expression. The `_with` variants take a `ForeignTable` the
caller resolved, so the driver can settle on a backend from the same
resolution the run then uses. `run_function` executes one named function
with no arguments. Integration tests use it to call a fixture function
directly.

The crate also exports `RuntimeError`, `Value`, `EnumPayload`,
`ForeignTable`, and `Unresolved`.

The input is always sealed. `koja-ir` enforces SSA definition before use,
terminator presence, and entry-point resolution before it hands back the
IR. The interpreter performs no program-level validation. A missing
value, missing entry point, or unterminated block is a seal violation
upstream and panics in `koja-ir::seal`, never here.

One walker drives both IR shapes. `IRProgram` and `IRScript` implement
the crate-private `CallResolver` trait, which supplies function, decl,
and constant lookup.

## Runtime errors

`RuntimeError` covers only conditions a program can reach at runtime
without a compiler bug:

- `Panicked { message }`: `Kernel.panic` calls and arithmetic faults
  (overflow, zero divisor, non-finite float result). Fault messages are
  the shared `koja_ir` constants, identical to the LLVM backend's panics.
- `TypeMismatch { detail }`: an operator received operands whose runtime
  types it cannot combine.
- `UnknownIntrinsic { symbol }`: an `@intrinsic` call reached a symbol
  with no handler in `intrinsics`. A missing registration, not a user
  error.
- `ExternUnresolved { c_name, link_lib, reason, symbol }`: an
  `@extern "C"` call reached a symbol with no shim in `externs` that the
  dynamic loader could not find either.
- `UnreachableExecuted`: control reached `IRTerminator::Unreachable`,
  so an upstream exhaustiveness or divergence judgment was wrong.
- `Unsupported { detail }`: a valid program the interpreter cannot run
  where the LLVM backend can.
- `ValueUndefined { id }`: defensive guard, unreachable on a sealed
  program.

## Module map

- `interpreter`: the `Interpreter` entry points, the `CallResolver`
  seam, frames, and the per-instruction walker.
- `value`: the runtime `Value` enum. Primitives map 1:1 onto
  `koja_ir::ConstValue`. Composite variants carry their receiver's
  `IRSymbol` and evaluated payloads.
- `ops`: pure operator math on resolved `Value` operands. Integer
  arithmetic runs at the operand type's width and signedness.
- `binary_ops`: `Binary`, `Bits`, and `String` byte and bit operations,
  including the `BinaryMatch` driver.
- `built_constants`: per-run store for `IRConstantValue::Built`
  constants. PID 1 runs every init in `built_constant_order` before user
  code starts.
- `intrinsics`: dispatch table for `@intrinsic` bodies, keyed by
  `IRIntrinsicId` and routed through an exhaustive `match`.
- `externs`: two-tier dispatch for `@extern "C"` bodies. The first tier
  is hand-written shims keyed by C symbol name. The second tier,
  `externs/foreign`, resolves the rest through `dlopen` / `dlsym` and
  calls them through libffi.
- `abi`: the rc-prefixed heap-block layout shared with the runtime for
  `String` and `Binary` payloads that cross the C boundary.
- `scheduler`: eval's cooperative implementation of the
  `koja-runtime-core` scheduler protocol. A process is an `async`
  interpreter future that the executor polls.
- `reactor`: eval's cooperative I/O reactor. Single-threaded, with
  thread-local registration state.

## Hard contracts

- Sealed input only. The interpreter trusts the seal and skips its own
  validation. A `ValueUndefined` is a seal violation to fix upstream, not
  a failure mode to handle here.
- Parity with native. Where a helper exists to match LLVM output byte
  for byte, its doc names the native counterpart.
- Externs run the same machine code as native. Each shim calls the same
  `koja-runtime` or libc symbol the LLVM backend links against.
