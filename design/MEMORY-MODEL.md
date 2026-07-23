# Memory Model

Koja has value semantics with automatic memory management. Every binding,
parameter, return, capture, and field behaves as an independent value. The
implementation may share storage when that sharing cannot be observed.

This document separates language semantics from backend representation. Binary
layouts shared with the native runtime are specified in [ABI.md](ABI.md).

## Semantic contract

1. Assignment produces an independent destination and leaves the source usable.
2. Function and closure parameters are passed by value.
3. Returning a value does not invalidate the source binding.
4. Reading a field leaves the containing value usable.
5. Changing one binding cannot change another binding.
6. Normal control-flow edges release managed values when their bindings leave
   scope.

There is no `move` keyword, borrow checker, lifetime syntax, user-visible
`clone`, or tracing garbage collector.

These guarantees apply to safe Koja-managed values. `CPtr<T>` exposes foreign
memory and follows the aliasing, lifetime, and synchronization rules of the
foreign API.

## Native representation

Inline primitives copy their bits.

`String`, `Binary`, and `Bits` use non-atomic reference-counted blocks. A copy
increments the block count. The final drop frees the allocation. Closures use a
reference-counted environment with drop and deep-copy functions for captures.

Collections use independent backing buffers. Copying a `List`, `Map`, or `Set`
allocates and copies its buffer, then acquires each managed element. Copy cost
is therefore proportional to collection size. User-defined composites recurse
through the representation of their fields rather than owning one universal
reference-counted block.

The compiler synthesizes clone and drop glue for managed composites. Physical
sharing inside leaves and closure environments is not observable as aliasing.

## Interpreter representation

The interpreter represents values with Rust values and host `Rc` storage.
`Clone`, `DeepCopy`, and `Drop` IR operations preserve Koja semantics but do
not reproduce native reference-count operations.

Interpreter collections may share host storage temporarily. Their functional
mutators copy the backing value before writing, so the sharing remains
unobservable. Backend parity concerns language behavior, not identical
allocation or reclamation mechanics.

## Functional mutation

Koja mutation is expressed by returning a new value and rebinding it. Current
collection mutators copy their backing buffer before writing. String
concatenation allocates a new block.

The language permits a future uniqueness proof to update storage in place, or
to remove redundant reference-count operations, when the result is
indistinguishable from an independent copy. General in-place-when-unique and
reference-count optimization are not implemented today.

## Scope and glue

Type checking decides value types and coercions. IR lowering inserts the
ownership-shaped operations required on normal control-flow edges.

- `Clone` creates the destination value using the type's native strategy.
- `DropLocal` and `DropValue` release managed values.
- Composite glue recursively processes managed fields and elements.
- Closure glue manages captured environments.
- `DeepCopy` marks a process-boundary semantic copy.

The elaborate pass registers required glue. IR sealing validates the resulting
program shape. Missing or malformed glue is a compiler failure, not a
source-language operation or diagnostic.

## Process boundaries

Process values are isolated. Sending, scheduling, replying, and spawning cannot
let a mutation in one process affect another process.

The native backend enforces this by physically deep-copying managed payloads
before transport. This keeps non-atomic reference-counted blocks process-local.
The interpreter runs logical processes on one thread and may share host `Rc`
storage while preserving isolation through functional mutation.

Native transport owns a copied payload until one of two outcomes.

- Delivery transfers the payload into the receiver.
- Discard runs payload drop glue before freeing the transport.

Discard includes a dead target, an expired reply, a cancelled timer, and
process death with queued mail. Queued mail is ownership-correct, but unbounded
mailboxes can still grow RSS when producers outrun consumers.

`CPtr<T>` is copied as a foreign pointer, not as the memory it references. It
can therefore name foreign storage shared by multiple processes. Such sharing
is outside Koja's managed-memory guarantees.

## Cycles

Safe managed values provide no mutable alias with which ordinary data can point
back into itself. Recursive layouts use `Indirect` storage, and construction
copies existing values. Koja therefore has no cycle collector.

This follows from the current safe surface rather than an independent
cycle-analysis pass. Foreign pointers are outside the guarantee. Closure
capture cycles are not separately specified or exhaustively tested.

## Concurrency

At most one worker executes a process at a time. Native managed values may use
non-atomic reference counts because native process-boundary transport
deep-copies them. A process may migrate between workers only while it is not
executing.

Koja exposes no safe managed shared mutable heap. Shared application state is
modeled through processes and typed messages. Foreign pointers can bypass this
model and require external synchronization.

## Failure and forced termination

Allocation failure is unrecoverable and aborts the native program.

A user panic is contained to one process. Native functions carry unwind tables
so the panic can cross compiled frames to the runtime catch boundary. The
compiler does not currently emit unwind cleanup landing pads, so managed values
held only by active native frames are not guaranteed to be released on panic.

Forced kill does not run the process's handler or active frame cleanup. The
runtime reclaims the process stack, initial config, queued transports, and
other runtime-owned state. Managed allocations referenced only by discarded
native frames are not currently recoverable and may remain until OS process
exit.

Normal return and normal scope edges do run compiler-generated drop glue. Eval
uses Rust value and future destruction for its own host representation, so its
failure-path mechanics are not identical to native execution.

## Verification

Automated coverage has distinct layers.

- Language fixtures check value-semantics behavior on both backends.
- LLVM memory fixtures use `koja_rt_live_blocks` to check the native allocator
  returns to a baseline after selected patterns.
- Compiler tests check glue discovery, synthesis, and sealing.
- Process fixtures cover dead targets, stale replies, queued mail, spawned
  process state, and recursive indirect reclamation.

The native live-block oracle does not comprehensively measure interpreter host
memory. Panic and forced-kill cleanup of live native frame locals are not
covered by a passing reclamation fixture.

HTTP, scheduler, and process-churn soaks under `benchmarks/soak/` are manual
endurance evidence. They are not default CI gates and do not promise bounded
RSS under an unbounded mailbox backlog.

Implementation history is preserved in
[archive/20260607-MEMORY-MODEL-RC-ROLLOUT.md](archive/20260607-MEMORY-MODEL-RC-ROLLOUT.md).
The superseded affine model remains in
[archive/20260607-OWNERSHIP-DROP.md](archive/20260607-OWNERSHIP-DROP.md).
