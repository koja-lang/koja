# Streams and Indefinite Iteration

Status: design draft. Finite cursor-based enumeration is implemented. Streams
and `while ... in` are not implemented.

## Summary

Koja separates finite traversal from sources that can wait or continue
indefinitely:

| Loop            | Protocol                 | Contract                         |
| --------------- | ------------------------ | -------------------------------- |
| `for x in xs`   | `Enumeration<T, Cursor>` | Finite, replayable traversal     |
| `while x in xs` | `Stream<T>`              | No termination or replay promise |

`Enumeration` uses an external cursor. The source remains unchanged:

```koja
protocol Enumeration<T, Cursor>
  fn cursor(self) -> Cursor
  fn next(self, cursor: Cursor) -> Option<(T, Cursor)>
end
```

List, String, Range, Map, and Set implement this protocol. Map and Set yield
their contents in unspecified order.

The proposed `Stream<T>` protocol represents a source that can wait or continue
indefinitely:

```koja
protocol Stream<T>
  fn next(self) -> Option<(T, Self)>
end
```

The loop keyword states the source contract. `for` consumes finite data.
`while ... in` consumes an indefinite stream.

## Finite enumeration

### Why use an external cursor

An external cursor supports sequential and hash-based collections without
requiring indexed access. The source and cursor have separate types and values.

```koja
cursor = scores.cursor()

loop
  match scores.next(cursor)
    Option.Some(((name, score), rest)) ->
      cursor = rest
      "#{name}: #{score}".print()

    Option.None -> break
  end
end
```

`for` provides this rewrite:

```koja
for (name, score) in scores
  "#{name}: #{score}".print()
end
```

The compiler evaluates `scores` once, stores its initial cursor, and advances
that cursor until `next` returns `None`.

### Conformance

`for` requires declared `Enumeration<T, Cursor>` conformance. Functions named
`cursor` and `next` do not provide structural conformance.

The protocol accepts a cursor type parameter because each source owns its
cursor representation:

- List and Map use an integer slot.
- String uses a UTF-8 byte offset.
- Range uses an optional integer to represent completion without overflow.

Generic code can name both protocol arguments:

```koja
fn count<T, Cursor, E: Enumeration<T, Cursor>>(source: E) -> Int
  result = 0

  for _ in source
    result += 1
  end

  result
end
```

Parameterized bounds remain compile-time facts. Koja resolves each call
statically and does not add vtables or runtime protocol values.

### Termination contract

`Enumeration` states that traversal is finite. The compiler does not prove this
from the implementation of `next`.

A dishonest conformance can return `Some` forever, just as a dishonest
`Equality` conformance can violate symmetry. Standard library conformances must
end for every valid cursor.

## Indefinite streams

### Motivation

Sockets, process mailboxes, timers, and generated sequences do not have the
same contract as finite collections. They can wait, fail, or continue forever.

Routing these sources through `for` would hide that behavior. The proposed
`while ... in` form makes indefinite consumption visible at the loop header.

### Proposed loop

```koja
while event in events
  handle(event)
end
```

Grammar: `while <pattern> in <expr>`.

The pattern binds the stream item and must be irrefutable. A failed pattern
never ends the loop. Only stream completion ends it.

The basic rewrite is:

```koja
loop
  match stream.next()
    Option.Some((item, rest)) ->
      stream = rest
      handle(item)

    Option.None -> break
  end
end
```

The compiler must evaluate a non-variable operand once. If the operand is a
variable, the design must define whether the advanced stream remains visible
after the loop.

### Value semantics do not imply resource duplication

Copying a pure stream can create two independent traversal positions. This
behavior does not apply automatically to sockets or process mailboxes.

An external resource can have one consumptive queue even when several Koja
values refer to it. A stream type must document whether copies replay data,
share consumption, or are rejected by its API design.

The language must not claim that every stream copy is an independent fork.

### Errors and waiting

`Option.None` can represent normal completion. It cannot also explain I/O
failure or cancellation without losing information.

The stream design must specify these cases before implementation:

- Fallible `next`, likely through Koja's `!` channel.
- Blocking and asynchronous waiting.
- Cancellation while suspended.
- Cleanup when `break`, `return`, or `try` exits the loop.
- Ownership and closure of external resources.

A suspending `while ... in` could cover part of the same surface as `receive`.
That unification is promising, but it needs process mailbox and timeout
semantics before it can replace a dedicated receive form.

## Examples

Finite Map traversal uses `for`:

```koja
for (key, value) in scores
  "#{key}: #{value}".print()
end
```

An infinite pure stream needs an explicit exit:

```koja
struct Fib: Stream<Int>
  a: Int
  b: Int

  fn next(self) -> Option<(Int, Self)>
    Option.Some((self.a, Fib{a: self.b, b: self.a + self.b}))
  end
end

fib = Fib{a: 0, b: 1}

while n in fib
  if n > 1000
    break
  end

  n.print()
end
```

A future fallible socket stream could use `try`:

```koja
while line in try socket.lines()
  handle(line)
end
```

This syntax is illustrative. The final error and suspension contracts remain
open.

## Implemented standard library work

The finite iteration phase includes:

1. Cursor-based Enumeration for List, String, Range, Map, and Set.
2. String, Map, and Set cursor intrinsics in both backends.
3. Nominal conformance checks for `for`.
4. Parameterized protocol bounds for generic code.
5. Content-based Map and Set equality and Debug output.

String cursors use UTF-8 byte offsets, so a complete traversal is linear in
the string byte length.

## Rejected designs

### Indexed Enumeration

The old `length` and `get(index)` protocol excluded hash tables and made String
traversal need repeated scans. These functions remain collection APIs where
indexed access is useful, but they no longer define iteration.

### A self-consuming iterator for finite collections

`next(self) -> Option<(T, Self)>` works for streams but couples finite data to
its traversal state. An external cursor keeps a replayable source separate
from one traversal.

### Internal iteration

An `each` function that takes a closure complicates `break`, early `return`,
and `try` in the body. External iteration keeps the body in normal statement
position.

### Structural loop conformance

Accepting any type with functions named `cursor` and `next` weakens the finite
contract and makes accidental conformance possible. `for` requires a declared
protocol conformance.

## Deferred work

- Complete the Stream error, cancellation, and resource contracts.
- Decide whether `while ... in` suspends directly.
- Decide how stream operands remain visible after the loop.
- Add stream adapters such as `map`, `filter`, and `take`.
- Evaluate generator syntax that lowers to Stream.
- Compare a suspending stream loop with the proposed `receive` surface.

## Open questions

1. Should a fallible stream return through `!`, or should the item type contain
   the error?
2. How does cancellation close a resource while `next` is suspended?
3. Can a stream declare replayable or consumptive behavior in its type?
4. Does `while ... in` advance a visible variable or a hidden binding?
5. Can mailbox streams preserve selective receive and timeout behavior?
